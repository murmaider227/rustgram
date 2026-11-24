//! End-to-end scripted fake-server conformance tests for Telegram 1:1 voice/video calls and
//! group calls (`tg-calls`, `tg-api`, `tg-session`, `tg-mtproto`, `tg-tl`) against an in-process
//! MTProto 2.0 fake server matching Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Zero network access; uses fake credentials (`api_id = 1`, `api_hash = "0000...0000"`) and an
//! ephemeral test-only RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{ApiCredentials, InvocationError, RpcClient, UpdateEvent, UpdatesHandler};
use tg_calls::call::{CallAction, CallState, CallTimerKind, ControllerState};
use tg_calls::dh::{
    DH_GOOD_PRIME, DhConfig, RANDOM_POWER_SIZE, compute_ga_hash, finish_dh_key_exchange,
    generate_mod_exp_first_with_rng,
};
use tg_calls::emoji_fingerprint::compute_call_emoji_fingerprint;
use tg_calls::group::participant::VersionedGroupCallUpdate;
use tg_calls::group::payload::{DtlsFingerprint, GroupJoinPayload, JoinSsrcGroup};
use tg_calls::group::state::{
    GroupCallAction, GroupCallSession, GroupCallState, GroupCallTimerKind, MuteState,
};
use tg_calls::instance::{CallsInstance, InstanceAction};
use tg_calls::protocol::{CallProtocolConfig, ServerCallConfig};
use tg_calls::signaling::SignalingCipher;
use tg_calls::tl_conv::{PhoneGroupCallResponse, PhoneGroupCallStreamChannelsResponse, peer_to_tl};
use tg_calls::types::{
    CallId, DhConfigResponse, GroupCallObject, GroupCallParticipantData, PeerId,
    PhoneCallDiscardReason, PhoneCallUpdate, UpdateGroupCallParticipants,
    UpdatePhoneCallSignalingData, UserId,
};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::Session;
use tg_mtproto::dc_options::DcId;
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, PacketConnection, TransportFactory,
};
use tg_session::{MtprotoRpc, build_mtproto_client_config};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, RawObject, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::mpsc;

// ============================================================================
// Test-only constants & scripted MTProto 2.0 fake server
// ============================================================================

const FAKE_API_ID: i32 = 1;
const FAKE_API_HASH: &str = "00000000000000000000000000000000";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

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
    let bytes = hex.as_bytes();
    assert_eq!(bytes.len() % 2, 0);
    (0..bytes.len())
        .step_by(2)
        .map(|i| {
            let hi = (bytes[i] as char).to_digit(16).expect("hex hi");
            let lo = (bytes[i + 1] as char).to_digit(16).expect("hex lo");
            ((hi << 4) | lo) as u8
        })
        .collect()
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

#[derive(Debug, Clone)]
struct IncomingRpc {
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

    fn bool_true() -> Self {
        Self::Reply(BOOL_TRUE_ID.to_le_bytes().to_vec())
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

struct LiveConnPush {
    dc_id: DcId,
    tx: mpsc::UnboundedSender<tl_enums::Updates>,
}

struct ScriptedServerInner {
    rsa_pub: PublicKey,
    rsa_n: Vec<u8>,
    rsa_d: Vec<u8>,
    next_conn_id: AtomicU64,
    dh_counter: AtomicU64,
    keys: Mutex<HashMap<u64, ServerKeyEntry>>,
    rpcs: Mutex<Vec<IncomingRpc>>,
    live_pushers: Mutex<HashMap<u64, LiveConnPush>>,
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
                live_pushers: Mutex::new(HashMap::new()),
                handler: Mutex::new(Arc::new(handler)),
            }),
        }
    }

    fn register_rsa_key_on(&self, client: &Client) {
        client
            .dc_options()
            .add_public_key(self.inner.rsa_pub.clone());
    }

    fn rpcs(&self) -> Vec<IncomingRpc> {
        self.inner.rpcs.lock().unwrap().clone()
    }

    fn push_update_to_dc(&self, dc_id: DcId, updates: tl_enums::Updates) -> bool {
        let guard = self.inner.live_pushers.lock().unwrap();
        if let Some((_, pusher)) = guard
            .iter()
            .filter(|(_, p)| p.dc_id == dc_id)
            .max_by_key(|&(&cid, _)| cid)
        {
            pusher.tx.send(updates).is_ok()
        } else {
            false
        }
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
        let (push_tx, mut push_rx) = mpsc::unbounded_channel::<tl_enums::Updates>();
        server
            .inner
            .live_pushers
            .lock()
            .unwrap()
            .insert(conn_id, LiveConnPush { dc_id, tx: push_tx });

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
            let mut active_key: Option<(AuthKey, u64, u64)> = None;
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

            loop {
                tokio::select! {
                    Some(pushed) = push_rx.recv() => {
                        if let Some((ref key, salt, session_id)) = active_key {
                            let u_id = next_server_msg_id(&mut server_msg_counter, false);
                            send_encrypted(
                                &peer,
                                key,
                                salt,
                                session_id,
                                u_id,
                                true,
                                &mut server_seq_counter,
                                &pushed.to_bytes(),
                            );
                        }
                    }
                    maybe_raw = peer.rx.recv() => {
                        let Some(raw) = maybe_raw else { break; };
                        if raw.len() < 8 { break; }
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
                                server_nonce[8..16].copy_from_slice(&0xA55A_C33C_1234_5678u64.to_le_bytes());
                                let res_pq = mtp_enums::ResPQ::ResPQ(mtp_types::ResPQ {
                                    nonce: req.nonce,
                                    server_nonce,
                                    pq: vec![0x01, 0x87],
                                    server_public_key_fingerprints: vec![
                                        server.inner.rsa_pub.fingerprint() as i64,
                                    ],
                                });
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let _ = peer.tx.send(Ok(encode_unencrypted_packet(out_id, &res_pq.to_bytes())));
                            } else if ctor == mtp_functions::ReqDHParams::CONSTRUCTOR_ID {
                                let req = mtp_functions::ReqDHParams::from_bytes(body).unwrap();
                                let plain = server.decrypt_rsa_pad(&req.encrypted_data);
                                let mut plain_cur = Cursor::new(&plain);
                                let pq_inner = mtp_enums::PQInnerData::deserialize(&mut plain_cur).unwrap();
                                let new_nonce = match pq_inner {
                                    mtp_enums::PQInnerData::Dc(d) => d.new_nonce,
                                    mtp_enums::PQInnerData::TempDc(d) => d.new_nonce,
                                    other => panic!("unexpected PQInnerData: {other:?}"),
                                };
                                let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                                let mut a_srv = [0x42u8; 256];
                                a_srv[0..8].copy_from_slice(&seq.to_be_bytes());
                                a_srv[248..256].copy_from_slice(&(seq ^ 0x55AA_55AA).to_be_bytes());
                                let g_a = pad_be_256(&mod_exp(&[3], &a_srv, &GOOD_PRIME).unwrap());
                                let inner = mtp_enums::ServerDHInnerData::ServerDHInnerData(
                                    mtp_types::ServerDHInnerData {
                                        nonce: req.nonce,
                                        server_nonce: req.server_nonce,
                                        g: 3,
                                        dh_prime: GOOD_PRIME.to_vec(),
                                        g_a: g_a.to_vec(),
                                        server_time: now_unix_secs(),
                                    },
                                ).to_bytes();
                                let digest = sha1(&[&inner]);
                                let mut answer_with_hash = Vec::with_capacity(20 + inner.len() + 16);
                                answer_with_hash.extend_from_slice(&digest);
                                answer_with_hash.extend_from_slice(&inner);
                                while answer_with_hash.len() % 16 != 0 {
                                    answer_with_hash.push(0);
                                }
                                let (aes_key, aes_iv) = dh_server_aes_key_iv(&new_nonce, &req.server_nonce);
                                let encrypted_answer = aes_ige_encrypt(&answer_with_hash, &aes_key, &aes_iv);
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
                                let _ = peer.tx.send(Ok(encode_unencrypted_packet(out_id, &resp.to_bytes())));
                            } else if ctor == mtp_functions::SetClientDHParams::CONSTRUCTOR_ID {
                                let req = mtp_functions::SetClientDHParams::from_bytes(body).unwrap();
                                let pending = dh_pending.take().unwrap();
                                let dec = aes_ige_decrypt(&req.encrypted_data, &pending.aes_key, &pending.aes_iv);
                                let mut dec_cur = Cursor::new(&dec[20..]);
                                let mtp_enums::ClientDHInnerData::ClientDHInnerData(cdh) =
                                    mtp_enums::ClientDHInnerData::deserialize(&mut dec_cur).unwrap();
                                let shared = pad_be_256(&mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME).unwrap());
                                let auth_key = AuthKey::new(shared);
                                let nn_lo = u64::from_le_bytes(pending.new_nonce[0..8].try_into().unwrap());
                                let sn_lo = u64::from_le_bytes(pending.server_nonce[0..8].try_into().unwrap());
                                let server_salt = nn_lo ^ sn_lo;
                                let nh1 = dh_new_nonce_hash(&pending.new_nonce, 1, &auth_key.aux_hash());
                                server.inner.keys.lock().unwrap().insert(
                                    auth_key.key_id_u64(),
                                    ServerKeyEntry { key: auth_key, server_salt },
                                );
                                let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(mtp_types::DhGenOk {
                                    nonce: pending.nonce,
                                    server_nonce: pending.server_nonce,
                                    new_nonce_hash1: nh1,
                                });
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let _ = peer.tx.send(Ok(encode_unencrypted_packet(out_id, &ok.to_bytes())));
                            }
                            continue;
                        }

                        let Some(entry) = server.inner.keys.lock().unwrap().get(&auth_key_id).cloned() else {
                            let _ = peer.tx.send(Err(TransportError::ServerError(
                                TransportErrorCode::AuthKeyNotFound,
                            )));
                            break;
                        };
                        let dec = parse_encrypted_packet(&raw, &entry.key, Side::Client).unwrap();
                        let session_id = dec.session_id;
                        active_key = Some((entry.key.clone(), entry.server_salt, session_id));

                        let inner_msgs: Vec<(u64, Vec<u8>)> = if dec.body.len() >= 4
                            && u32::from_le_bytes(dec.body[0..4].try_into().unwrap()) == MsgContainer::CONSTRUCTOR_ID
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
                                && u32::from_le_bytes(raw_msg[0..4].try_into().unwrap()) == GzipPacked::CONSTRUCTOR_ID
                            {
                                unwrap_gzip_layers(&raw_msg, &mut budget, 0).unwrap()
                            } else {
                                raw_msg
                            };
                            if body.len() < 4 { continue; }
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
                                    send_encrypted(&peer, &entry.key, entry.server_salt, session_id, ns_id, true, &mut server_seq_counter, &ns.to_bytes());
                                }
                                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                                    msg_id: msg_id as i64,
                                    ping_id: ping.ping_id,
                                });
                                let p_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(&peer, &entry.key, entry.server_salt, session_id, p_id, false, &mut server_seq_counter, &pong.to_bytes());
                                continue;
                            }
                            if outer_ctor == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID {
                                let ping = mtp_functions::PingDelayDisconnect::from_bytes(&body).unwrap();
                                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                                    msg_id: msg_id as i64,
                                    ping_id: ping.ping_id,
                                });
                                let p_id = next_server_msg_id(&mut server_msg_counter, true);
                                send_encrypted(&peer, &entry.key, entry.server_salt, session_id, p_id, false, &mut server_seq_counter, &pong.to_bytes());
                                continue;
                            }
                            if outer_ctor == mtp_types::MsgsAck::CONSTRUCTOR_ID
                                || outer_ctor == mtp_functions::GetFutureSalts::CONSTRUCTOR_ID
                            {
                                continue;
                            }

                            let inner_body = unwrap_init_if_present(&body);
                            let inner_ctor = u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                            let rpc = IncomingRpc { ctor: inner_ctor, body: inner_body };
                            server.inner.rpcs.lock().unwrap().push(rpc.clone());

                            let handler = server.inner.handler.lock().unwrap().clone();
                            let action = handler(&server, rpc);
                            let payload = match action {
                                ServerAction::Reply(p) => p,
                                ServerAction::RpcError(code, msg) => {
                                    mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                        error_code: code,
                                        error_message: msg,
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
                            send_encrypted(&peer, &entry.key, entry.server_salt, session_id, r_id, true, &mut server_seq_counter, &rpc_res);
                        }
                    }
                }
            }
            server.inner.live_pushers.lock().unwrap().remove(&conn_id);
        });
    }
}

fn unwrap_init_if_present(raw: &[u8]) -> Vec<u8> {
    if raw.len() < 4 {
        return raw.to_vec();
    }
    let ctor = u32::from_le_bytes(raw[0..4].try_into().unwrap());
    if ctor != tl_fns::InvokeWithLayer::<RawObject>::CONSTRUCTOR_ID {
        return raw.to_vec();
    }
    let mut cur = Cursor::new(&raw[4..]);
    let _layer = i32::deserialize(&mut cur).unwrap();
    let rest = &raw[raw.len() - cur.remaining()..];
    let mut icur = Cursor::new(&rest[4..]);
    let flags = i32::deserialize(&mut icur).unwrap() as u32;
    let _api_id = i32::deserialize(&mut icur).unwrap();
    let _device = String::deserialize(&mut icur).unwrap();
    let _sys = String::deserialize(&mut icur).unwrap();
    let _app = String::deserialize(&mut icur).unwrap();
    let _slang = String::deserialize(&mut icur).unwrap();
    let _lpack = String::deserialize(&mut icur).unwrap();
    let _lcode = String::deserialize(&mut icur).unwrap();
    if (flags & 1) != 0 {
        let _ = tl_enums::InputClientProxy::deserialize(&mut icur).unwrap();
    }
    if (flags & 2) != 0 {
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

fn test_client_config(main_dc: DcId) -> ClientConfig {
    let creds = ApiCredentials {
        api_id: FAKE_API_ID,
        api_hash: FAKE_API_HASH.to_owned(),
    };
    let mut cfg =
        build_mtproto_client_config(&creds, main_dc, tg_core::Environment::Production, None);
    cfg.proxy = ProxyData::none();
    cfg.use_pfs = false;
    cfg.ping_interval = Duration::from_secs(600);
    cfg
}

fn tl_config_with_call_timeouts(
    receive_ms: i32,
    ring_ms: i32,
    connect_ms: i32,
    packet_ms: i32,
) -> tl_enums::Config {
    let now = now_unix_secs();
    tl_enums::Config::Config(tl_types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: false,
        revoke_pm_inbox: true,
        blocked_mode: false,
        force_try_ipv6: false,
        date: now,
        expires: now + 3600,
        test_mode: false,
        this_dc: 2,
        dc_options: vec![],
        dc_txt_domain_name: "apv3.stel.com".to_owned(),
        chat_size_max: 200,
        megagroup_size_max: 200_000,
        forwarded_count_max: 100,
        online_update_period_ms: 210_000,
        offline_blur_timeout_ms: 5_000,
        offline_idle_timeout_ms: 30_000,
        online_cloud_timeout_ms: 300_000,
        notify_cloud_delay_ms: 30_000,
        notify_default_delay_ms: 1_500,
        push_chat_period_ms: 60_000,
        push_chat_limit: 2,
        edit_time_limit: 172_800,
        revoke_time_limit: 172_800,
        revoke_pm_time_limit: 172_800,
        rating_e_decay: 2_419_200,
        stickers_recent_limit: 50,
        channels_read_media_period: 604_800,
        tmp_sessions: Some(0),
        call_receive_timeout_ms: receive_ms,
        call_ring_timeout_ms: ring_ms,
        call_connect_timeout_ms: connect_ms,
        call_packet_timeout_ms: packet_ms,
        me_url_prefix: "https://t.me/".to_owned(),
        autoupdate_url_prefix: None,
        gif_search_username: Some("gif".to_owned()),
        venue_search_username: None,
        img_search_username: None,
        static_maps_provider: None,
        caption_length_max: 1024,
        message_length_max: 4096,
        webfile_dc_id: 4,
        suggested_lang_code: Some("en".to_owned()),
        lang_pack_version: Some(1),
        base_lang_pack_version: Some(1),
        reactions_default: None,
        autologin_token: None,
    })
}

fn tl_state(pts: i32, seq: i32) -> tl_enums::updates::State {
    tl_enums::updates::State::State(tl_types::updates::State {
        pts,
        qts: 0,
        date: now_unix_secs(),
        seq,
        unread_count: 0,
    })
}

fn tl_empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![],
        users: vec![],
        chats: vec![],
        date: now_unix_secs(),
        seq: 0,
    })
}

fn tl_group_call_participant(
    user_id: i64,
    source: u32,
    muted: bool,
    versioned: bool,
    is_self: bool,
) -> tl_enums::GroupCallParticipant {
    tl_enums::GroupCallParticipant::GroupCallParticipant(tl_types::GroupCallParticipant {
        muted,
        left: false,
        can_self_unmute: true,
        just_joined: false,
        versioned,
        min: false,
        muted_by_you: false,
        volume_by_admin: false,
        self_: is_self,
        video_joined: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id }),
        date: 1_700_000_000,
        active_date: None,
        source: source as i32,
        volume: Some(10_000),
        about: None,
        raise_hand_rating: None,
        video: None,
        presentation: None,
        paid_stars_total: None,
    })
}

fn tl_group_call_info(id: i64, access_hash: i64, version: i32, count: i32) -> tl_enums::GroupCall {
    tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
        join_muted: false,
        can_change_join_muted: true,
        join_date_asc: false,
        schedule_start_subscribed: false,
        can_start_video: true,
        record_video_active: false,
        rtmp_stream: false,
        listeners_hidden: false,
        conference: false,
        creator: true,
        messages_enabled: true,
        can_change_messages_enabled: true,
        min: false,
        id,
        access_hash,
        participants_count: count,
        title: Some("MTProto Group Call".to_owned()),
        stream_dc_id: Some(4),
        record_start_date: None,
        schedule_date: None,
        unmuted_video_limit: 30,
        version,
        invite_link: None,
        send_paid_messages_stars: None,
        unmuted_video_count: None,
        default_send_as: None,
    })
}

// ============================================================================
// 1. End-to-End 1:1 Outgoing + Incoming Call over Scripted MTProto 2.0 Server
// ============================================================================

#[tokio::test]
async fn end_to_end_1to1_calls_dh_cache_signaling_and_rating_over_mtproto() {
    let dh_cfg = DhConfig {
        version: 14,
        g: 3,
        p: DH_GOOD_PRIME.to_vec(),
    };
    let remote_bob =
        generate_mod_exp_first_with_rng(&dh_cfg, &[0x55; 256], |b| b.fill(0x66)).expect("bob g_b");
    let remote_bob_gb = remote_bob.modexp.clone();
    let remote_bob_power = remote_bob.random_power;

    // Shared server state to cross-check wire fields across RPCs.
    #[derive(Default)]
    struct ServerCallState {
        dh_requests: Vec<(i32, i32)>,
        committed_ga_hash: Vec<u8>,
        confirmed_ga: Vec<u8>,
        confirmed_fp: i64,
        signaling_sent: Vec<Vec<u8>>,
        discard_seen: Option<(bool, i32, tl_enums::PhoneCallDiscardReason, i64)>,
        rating_seen: Option<(bool, i32, String)>,
        debug_seen: Option<String>,
        received_call_ids: Vec<i64>,
        accepted_gb: Vec<u8>,
    }

    let srv_state = Arc::new(Mutex::new(ServerCallState::default()));
    let srv_state_cb = Arc::clone(&srv_state);
    let bob_gb_cb = remote_bob_gb.clone();
    let dh_cfg_cb = dh_cfg.clone();

    let server = ScriptedServer::new(move |_srv, rpc| {
        match rpc.ctor {
            c if c == tl_fns::help::GetConfig::CONSTRUCTOR_ID => {
                // Custom timeouts to verify `ServerCallConfig` wiring.
                ServerAction::ok(&tl_config_with_call_timeouts(
                    22_000, 85_000, 31_000, 11_000,
                ))
            }
            c if c == tl_fns::updates::GetState::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_state(1, 1))
            }
            c if c == tl_fns::messages::GetDhConfig::CONSTRUCTOR_ID => {
                let req = tl_fns::messages::GetDhConfig::from_bytes(&rpc.body).unwrap();
                assert_eq!(req.random_length, RANDOM_POWER_SIZE as i32);
                srv_state_cb
                    .lock()
                    .unwrap()
                    .dh_requests
                    .push((req.version, req.random_length));
                if req.version == 14 {
                    ServerAction::ok(&tl_enums::messages::DhConfig::NotModified(
                        tl_types::messages::DhConfigNotModified {
                            random: vec![0x99; RANDOM_POWER_SIZE],
                        },
                    ))
                } else {
                    ServerAction::ok(&tl_enums::messages::DhConfig::DhConfig(
                        tl_types::messages::DhConfig {
                            g: 3,
                            p: DH_GOOD_PRIME.to_vec(),
                            version: 14,
                            random: vec![0x77; RANDOM_POWER_SIZE],
                        },
                    ))
                }
            }
            c if c == tl_fns::phone::GetCallConfig::CONSTRUCTOR_ID => {
                ServerAction::ok(&tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: "{}".to_owned(),
                }))
            }
            c if c == tl_fns::phone::RequestCall::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::RequestCall::from_bytes(&rpc.body).unwrap();
                assert!(req.video, "video flag must be serialized on wire");
                assert_eq!(req.random_id, 0x1357_2468);
                assert_eq!(req.g_a_hash.len(), 32);
                let tl_enums::PhoneCallProtocol::PhoneCallProtocol(proto) = req.protocol.clone();
                assert!(proto.udp_p2p);
                assert!(proto.udp_reflector);
                assert_eq!(proto.min_layer, 65);
                assert_eq!(proto.max_layer, 92);
                assert_eq!(
                    proto.library_versions,
                    vec!["12.0.0", "9.0.0", "8.0.0", "7.0.0"],
                    "library_versions on wire must be newest-first (CollectVersionsForApi)"
                );
                srv_state_cb.lock().unwrap().committed_ga_hash = req.g_a_hash;

                let resp = tl_enums::phone::PhoneCall::PhoneCall(tl_types::phone::PhoneCall {
                    phone_call: tl_enums::PhoneCall::Waiting(tl_types::PhoneCallWaiting {
                        video: true,
                        id: 40_001,
                        access_hash: 0x0102_0304_0506_0708,
                        date: 1_700_000_000,
                        admin_id: 100,
                        participant_id: 200,
                        protocol: req.protocol,
                        receive_date: None,
                    }),
                    users: vec![],
                });
                ServerAction::ok(&resp)
            }
            c if c == tl_fns::phone::ConfirmCall::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::ConfirmCall::from_bytes(&rpc.body).unwrap();
                let mut st = srv_state_cb.lock().unwrap();
                assert_eq!(
                    compute_ga_hash(&req.g_a).to_vec(),
                    st.committed_ga_hash,
                    "confirmCall.g_a must match earlier requestCall.g_a_hash"
                );
                let (_srv_key, expected_fp) =
                    finish_dh_key_exchange(&dh_cfg_cb, &req.g_a, &remote_bob_power)
                        .expect("server/bob DH");
                assert_eq!(
                    req.key_fingerprint as u64, expected_fp,
                    "confirmCall.key_fingerprint must match low 64 bits LE of SHA1(key)"
                );
                st.confirmed_ga = req.g_a;
                st.confirmed_fp = req.key_fingerprint;

                let resp = tl_enums::phone::PhoneCall::PhoneCall(tl_types::phone::PhoneCall {
                    phone_call: tl_enums::PhoneCall::PhoneCall(tl_types::PhoneCall {
                        p2p_allowed: true,
                        video: true,
                        conference_supported: false,
                        id: 40_001,
                        access_hash: 0x0102_0304_0506_0708,
                        date: 1_700_000_000,
                        admin_id: 100,
                        participant_id: 200,
                        g_a_or_b: bob_gb_cb.clone(),
                        key_fingerprint: req.key_fingerprint,
                        protocol: req.protocol,
                        connections: vec![tl_enums::PhoneConnection::Webrtc(
                            tl_types::PhoneConnectionWebrtc {
                                turn: true,
                                stun: true,
                                id: 55,
                                ip: "127.0.0.1".to_owned(),
                                ipv6: String::new(),
                                port: 3478,
                                username: "u".to_owned(),
                                password: "p".to_owned(),
                            },
                        )],
                        start_date: 1_700_000_010,
                        custom_parameters: None,
                    }),
                    users: vec![],
                });
                ServerAction::ok(&resp)
            }
            c if c == tl_fns::phone::SendSignalingData::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::SendSignalingData::from_bytes(&rpc.body).unwrap();
                srv_state_cb.lock().unwrap().signaling_sent.push(req.data);
                ServerAction::bool_true()
            }
            c if c == tl_fns::phone::DiscardCall::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::DiscardCall::from_bytes(&rpc.body).unwrap();
                srv_state_cb.lock().unwrap().discard_seen =
                    Some((req.video, req.duration, req.reason, req.connection_id));
                ServerAction::ok(&tl_empty_updates())
            }
            c if c == tl_fns::phone::SetCallRating::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::SetCallRating::from_bytes(&rpc.body).unwrap();
                srv_state_cb.lock().unwrap().rating_seen =
                    Some((req.user_initiative, req.rating, req.comment));
                ServerAction::ok(&tl_empty_updates())
            }
            c if c == tl_fns::phone::SaveCallDebug::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::SaveCallDebug::from_bytes(&rpc.body).unwrap();
                let tl_enums::DataJSON::DataJSON(dj) = req.debug;
                srv_state_cb.lock().unwrap().debug_seen = Some(dj.data);
                ServerAction::bool_true()
            }
            c if c == tl_fns::phone::ReceivedCall::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::ReceivedCall::from_bytes(&rpc.body).unwrap();
                let tl_enums::InputPhoneCall::InputPhoneCall(ipc) = req.peer;
                srv_state_cb.lock().unwrap().received_call_ids.push(ipc.id);
                ServerAction::bool_true()
            }
            c if c == tl_fns::phone::AcceptCall::CONSTRUCTOR_ID => {
                let req = tl_fns::phone::AcceptCall::from_bytes(&rpc.body).unwrap();
                let tl_enums::InputPhoneCall::InputPhoneCall(ipc) = req.peer;
                srv_state_cb.lock().unwrap().accepted_gb = req.g_b;
                let resp = tl_enums::phone::PhoneCall::PhoneCall(tl_types::phone::PhoneCall {
                    phone_call: tl_enums::PhoneCall::Waiting(tl_types::PhoneCallWaiting {
                        video: false,
                        id: ipc.id,
                        access_hash: ipc.access_hash,
                        date: 1_700_000_100,
                        admin_id: 200,
                        participant_id: 100,
                        protocol: req.protocol,
                        receive_date: Some(1_700_000_101),
                    }),
                    users: vec![],
                });
                ServerAction::ok(&resp)
            }
            other => panic!("unexpected RPC ctor 0x{other:08x}"),
        }
    });

    let (client, mut updates_rx) =
        Client::with_transport(test_client_config(2), Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());

    let mut session = Session::new(tg_data::UserId(100));
    let mut updates_handler = UpdatesHandler::new();
    tokio::time::timeout(
        TEST_TIMEOUT,
        updates_handler.initialize(&rpc, &mut session, 1_000),
    )
    .await
    .unwrap()
    .unwrap();

    // 1. Fetch `help.getConfig` over MTProto and load timeouts into `CallsInstance`.
    let tl_enums::Config::Config(cfg) =
        tokio::time::timeout(TEST_TIMEOUT, rpc.invoke(&tl_fns::help::GetConfig {}))
            .await
            .unwrap()
            .unwrap();
    let server_call_cfg = ServerCallConfig {
        call_receive_timeout_ms: cfg.call_receive_timeout_ms as u64,
        call_ring_timeout_ms: cfg.call_ring_timeout_ms as u64,
        call_connect_timeout_ms: cfg.call_connect_timeout_ms as u64,
        call_packet_timeout_ms: cfg.call_packet_timeout_ms as u64,
    };
    assert_eq!(server_call_cfg.call_receive_timeout_ms, 22_000);
    assert_eq!(server_call_cfg.call_ring_timeout_ms, 85_000);

    let mut instance =
        CallsInstance::new(UserId(100), server_call_cfg, CallProtocolConfig::default());

    // 2. Start outgoing video call -> GetCallConfig + GetDhConfig(version=0, random_length=256).
    instance.start_outgoing_call(UserId(200), true, true, 1_000);
    let start_actions = instance.take_actions();
    let (dh_ver, dh_rand_len) = start_actions
        .iter()
        .find_map(|a| match a {
            InstanceAction::GetDhConfig {
                version,
                random_length,
            } => Some((*version, *random_length)),
            _ => None,
        })
        .expect("expected GetDhConfig");
    assert_eq!(dh_ver, 0);
    assert_eq!(dh_rand_len, 256);

    let dh_resp: tl_enums::messages::DhConfig = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetDhConfig {
            version: dh_ver,
            random_length: dh_rand_len,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    instance.handle_dh_config_response_with_rng(
        Ok(DhConfigResponse::from(dh_resp)),
        0x1357_2468,
        |buf| buf.fill(0x33),
    );
    let req_call_actions = instance.take_actions();
    let (video, random_id, g_a_hash, protocol) = req_call_actions
        .iter()
        .find_map(|a| match a {
            InstanceAction::Call(CallAction::RequestCall {
                video,
                random_id,
                g_a_hash,
                protocol,
                ..
            }) => Some((*video, *random_id, *g_a_hash, protocol.clone())),
            _ => None,
        })
        .expect("expected RequestCall");

    let req_call_resp: tl_enums::phone::PhoneCall = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::RequestCall {
            video,
            user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 200,
                access_hash: 0,
            }),
            random_id,
            g_a_hash: g_a_hash.to_vec(),
            protocol: protocol.into(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    instance
        .current_call_mut()
        .unwrap()
        .handle_request_call_result(Ok(PhoneCallUpdate::try_from(req_call_resp).unwrap()), 2_000);
    let wait_actions = instance.current_call_mut().unwrap().take_actions();
    assert!(wait_actions.iter().any(|a| matches!(
        a,
        CallAction::SetTimer {
            kind: CallTimerKind::DiscardByTimeout,
            duration_ms: 22_000,
        }
    )));

    // 3. Server pushes `updatePhoneCall(phoneCallWaiting{receive_date})` over MTProto ->
    //    routed via `UpdatesHandler` -> `UpdateEvent::CallUpdate` -> `Call` transitions to Ringing.
    let pushed_ring = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
            phone_call: tl_enums::PhoneCall::Waiting(tl_types::PhoneCallWaiting {
                video: true,
                id: 40_001,
                access_hash: 0x0102_0304_0506_0708,
                date: 1_700_000_000,
                admin_id: 100,
                participant_id: 200,
                protocol: CallProtocolConfig::default().to_tl_protocol().into(),
                receive_date: Some(1_700_000_002),
            }),
        }),
        date: now_unix_secs(),
    });
    assert!(server.push_update_to_dc(2, pushed_ring));
    let raw_msg = tokio::time::timeout(TEST_TIMEOUT, updates_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let raw_upd = raw_msg
        .updates
        .map(|b| *b)
        .unwrap_or_else(|| tl_enums::Updates::from_bytes(&raw_msg.body).unwrap());
    updates_handler
        .handle_updates(&rpc, &mut session, raw_upd, None, 2_500)
        .await
        .unwrap();
    let events = updates_handler.drain_events();
    let phone_call_upd = events
        .into_iter()
        .find_map(|e| match e {
            UpdateEvent::CallUpdate(u) => match *u {
                tl_enums::Update::PhoneCall(pc) => {
                    Some(PhoneCallUpdate::try_from(pc.phone_call).unwrap())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("UpdatesHandler must surface UpdateEvent::CallUpdate(PhoneCall)");
    instance.handle_call_update(phone_call_upd, 1_700_000_002, 2_500);
    assert_eq!(instance.current_call().unwrap().state(), CallState::Ringing);
    let ring_actions = instance.take_actions();
    assert!(ring_actions.iter().any(|a| matches!(
        a,
        InstanceAction::Call(CallAction::SetTimer {
            kind: CallTimerKind::DiscardByTimeout,
            duration_ms: 85_000,
        })
    )));

    // 4. Server pushes `updatePhoneCall(phoneCallAccepted{g_b})` -> client runs `phone.confirmCall`.
    let pushed_accepted = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
            phone_call: tl_enums::PhoneCall::Accepted(tl_types::PhoneCallAccepted {
                video: true,
                id: 40_001,
                access_hash: 0x0102_0304_0506_0708,
                date: 1_700_000_000,
                admin_id: 100,
                participant_id: 200,
                g_b: remote_bob_gb.clone(),
                protocol: CallProtocolConfig::default().to_tl_protocol().into(),
            }),
        }),
        date: now_unix_secs(),
    });
    assert!(server.push_update_to_dc(2, pushed_accepted));
    let raw_msg = tokio::time::timeout(TEST_TIMEOUT, updates_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let raw_upd = raw_msg
        .updates
        .map(|b| *b)
        .unwrap_or_else(|| tl_enums::Updates::from_bytes(&raw_msg.body).unwrap());
    updates_handler
        .handle_updates(&rpc, &mut session, raw_upd, None, 3_000)
        .await
        .unwrap();
    let accepted_upd = updates_handler
        .drain_events()
        .into_iter()
        .find_map(|e| match e {
            UpdateEvent::CallUpdate(u) => match *u {
                tl_enums::Update::PhoneCall(pc) => {
                    Some(PhoneCallUpdate::try_from(pc.phone_call).unwrap())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("expected PhoneCallAccepted");
    instance.handle_call_update(accepted_upd, 1_700_000_003, 3_000);
    let confirm_actions = instance.take_actions();
    let (peer, g_a, key_fp, proto) = confirm_actions
        .iter()
        .find_map(|a| match a {
            InstanceAction::Call(CallAction::ConfirmCall {
                peer,
                g_a,
                key_fingerprint,
                protocol,
            }) => Some((*peer, g_a.clone(), *key_fingerprint, protocol.clone())),
            _ => None,
        })
        .expect("expected ConfirmCall");

    let confirm_resp: tl_enums::phone::PhoneCall = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::ConfirmCall {
            peer: peer.into(),
            g_a: g_a.clone(),
            key_fingerprint: key_fp as i64,
            protocol: proto.into(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    instance
        .current_call_mut()
        .unwrap()
        .handle_confirm_call_result(Ok(PhoneCallUpdate::try_from(confirm_resp).unwrap()), 3_200);
    let start_ctrl_actions = instance.current_call_mut().unwrap().take_actions();
    let ctrl_desc = start_ctrl_actions
        .iter()
        .find_map(|a| match a {
            CallAction::StartController(desc) => Some(desc.clone()),
            _ => None,
        })
        .expect("expected StartController");
    assert_eq!(ctrl_desc.initialization_timeout_ms, 31_000);
    assert_eq!(ctrl_desc.receive_timeout_ms, 11_000);
    assert_eq!(ctrl_desc.version, "12.0.0");

    // Verify both sides agree on exact 256-byte auth_key and 4-emoji fingerprint.
    let (bob_key, bob_fp) =
        finish_dh_key_exchange(&dh_cfg, &g_a, &remote_bob_power).expect("bob key");
    assert_eq!(ctrl_desc.encryption_key, bob_key);
    assert_eq!(key_fp, bob_fp);
    assert_eq!(
        instance
            .current_call()
            .unwrap()
            .emoji_fingerprint()
            .unwrap(),
        compute_call_emoji_fingerprint(&bob_key, &g_a)
    );

    // 5. Bidirectional `phone.sendSignalingData` and `updatePhoneCallSignalingData` over MTProto.
    let mut alice_sig = SignalingCipher::new(bob_key, true);
    let mut bob_sig = SignalingCipher::new(bob_key, false);
    let out_sig = alice_sig.encrypt(b"ice-offer").unwrap();
    instance
        .current_call_mut()
        .unwrap()
        .emit_signaling_data(out_sig.clone());
    let sig_actions = instance.current_call_mut().unwrap().take_actions();
    let (sig_peer, sig_bytes) = sig_actions
        .iter()
        .find_map(|a| match a {
            CallAction::SendSignalingData { peer, data } => Some((*peer, data.clone())),
            _ => None,
        })
        .expect("expected SendSignalingData");
    let sig_ok: bool = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::SendSignalingData {
            peer: sig_peer.into(),
            data: sig_bytes,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(sig_ok);
    assert_eq!(
        bob_sig
            .decrypt(&srv_state.lock().unwrap().signaling_sent[0])
            .unwrap()
            .payload,
        b"ice-offer"
    );

    let in_sig = bob_sig.encrypt(b"ice-answer").unwrap();
    assert!(server.push_update_to_dc(
        2,
        tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::PhoneCallSignalingData(
                tl_types::UpdatePhoneCallSignalingData {
                    phone_call_id: 40_001,
                    data: in_sig.clone(),
                }
            ),
            date: now_unix_secs(),
        })
    ));
    let raw_sig_msg = tokio::time::timeout(TEST_TIMEOUT, updates_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let raw_sig = raw_sig_msg
        .updates
        .map(|b| *b)
        .unwrap_or_else(|| tl_enums::Updates::from_bytes(&raw_sig_msg.body).unwrap());
    updates_handler
        .handle_updates(&rpc, &mut session, raw_sig, None, 3_500)
        .await
        .unwrap();
    let sig_upd = updates_handler
        .drain_events()
        .into_iter()
        .find_map(|e| match e {
            UpdateEvent::CallUpdate(u) => match *u {
                tl_enums::Update::PhoneCallSignalingData(s) => {
                    Some(UpdatePhoneCallSignalingData::from(s))
                }
                _ => None,
            },
            _ => None,
        })
        .expect("UpdatesHandler must surface PhoneCallSignalingData");
    assert!(instance.handle_signaling_data(sig_upd));
    let recv_sig = instance
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            InstanceAction::Call(CallAction::ReceiveSignalingData(b)) => Some(b),
            _ => None,
        })
        .expect("expected ReceiveSignalingData");
    assert_eq!(alice_sig.decrypt(&recv_sig).unwrap().payload, b"ice-answer");

    // 6. Establish, hang up (`phone.discardCall`), then rate & save debug log over MTProto.
    instance
        .current_call_mut()
        .unwrap()
        .handle_controller_state_change(ControllerState::Established, 10_000);
    instance
        .current_call_mut()
        .unwrap()
        .set_preferred_relay_id(55);
    instance
        .current_call_mut()
        .unwrap()
        .set_debug_log(r#"{"rtt":24}"#.to_owned());
    let _ = instance.current_call_mut().unwrap().take_actions();
    instance
        .current_call_mut()
        .unwrap()
        .hangup(None, 25_400, false);
    let disc_actions = instance.current_call_mut().unwrap().take_actions();
    let (d_video, d_peer, d_dur, d_reason, d_conn_id) = disc_actions
        .iter()
        .find_map(|a| match a {
            CallAction::DiscardCall {
                video,
                peer,
                duration,
                reason,
                connection_id,
            } => Some((*video, *peer, *duration, reason.clone(), *connection_id)),
            _ => None,
        })
        .expect("expected DiscardCall");
    let _: tl_enums::Updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::DiscardCall {
            video: d_video,
            peer: d_peer.into(),
            duration: d_dur,
            reason: d_reason.into(),
            connection_id: d_conn_id,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        srv_state.lock().unwrap().discard_seen,
        Some((true, 15, tl_enums::PhoneCallDiscardReason::Hangup, 55))
    );

    // Server pushes `phoneCallDiscarded(need_rating=true, need_debug=true)`.
    instance.handle_call_update(
        PhoneCallUpdate::Discarded(tg_calls::types::PhoneCallDiscarded {
            need_rating: true,
            need_debug: true,
            video: true,
            id: CallId(40_001),
            reason: Some(PhoneCallDiscardReason::Hangup),
            duration: Some(15),
        }),
        1_700_000_025,
        25_600,
    );
    let post_disc_actions = instance.take_actions();
    let (dbg_peer, dbg_json) = post_disc_actions
        .iter()
        .find_map(|a| match a {
            InstanceAction::Call(CallAction::SaveCallDebug { peer, debug_json }) => {
                Some((*peer, debug_json.clone()))
            }
            _ => None,
        })
        .expect("expected immediate SaveCallDebug");
    let dbg_ok: bool = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::SaveCallDebug {
            peer: dbg_peer.into(),
            debug: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: dbg_json }),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(dbg_ok);
    assert_eq!(
        srv_state.lock().unwrap().debug_seen.as_deref(),
        Some(r#"{"rtt":24}"#)
    );

    instance
        .current_call_mut()
        .unwrap()
        .submit_rating(5, "great call".to_owned());
    let rate_actions = instance.current_call_mut().unwrap().take_actions();
    let (r_init, r_peer, r_val, r_comment) = rate_actions
        .iter()
        .find_map(|a| match a {
            CallAction::SetCallRating {
                user_initiative,
                peer,
                rating,
                comment,
            } => Some((*user_initiative, *peer, *rating, comment.clone())),
            _ => None,
        })
        .expect("expected SetCallRating");
    let _: tl_enums::Updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::SetCallRating {
            user_initiative: r_init,
            peer: r_peer.into(),
            rating: r_val,
            comment: r_comment,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        srv_state.lock().unwrap().rating_seen,
        Some((false, 5, "great call".to_owned()))
    );
    instance.destroy_current_call();

    // 7. Second (incoming) call reuses cached DH version = 14 -> `DhConfigNotModified`,
    //    sends `phone.receivedCall` and `phone.acceptCall`.
    let caller_ga = generate_mod_exp_first_with_rng(&dh_cfg, &[0xAA; 256], |b| b.fill(0xBB))
        .expect("caller g_a");
    instance.handle_call_update(
        PhoneCallUpdate::Requested(tg_calls::types::PhoneCallRequested {
            video: false,
            id: CallId(40_002),
            access_hash: 0x090A,
            date: 1_700_000_100,
            admin_id: UserId(200),
            participant_id: UserId(100),
            g_a_hash: compute_ga_hash(&caller_ga.modexp).to_vec(),
            protocol: CallProtocolConfig::default().to_tl_protocol(),
        }),
        1_700_000_101,
        30_000,
    );
    let inc_actions = instance.take_actions();
    let cached_ver = inc_actions
        .iter()
        .find_map(|a| match a {
            InstanceAction::GetDhConfig { version, .. } => Some(*version),
            _ => None,
        })
        .expect("expected GetDhConfig for incoming call");
    assert_eq!(cached_ver, 14, "second call must send cached DH version 14");

    let dh_not_mod: tl_enums::messages::DhConfig = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetDhConfig {
            version: cached_ver,
            random_length: RANDOM_POWER_SIZE as i32,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        dh_not_mod,
        tl_enums::messages::DhConfig::NotModified(_)
    ));
    instance.handle_dh_config_response_with_rng(Ok(DhConfigResponse::from(dh_not_mod)), 1, |b| {
        b.fill(0x44)
    });
    let recv_peer = instance
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            InstanceAction::Call(CallAction::ReceivedCall { peer }) => Some(peer),
            _ => None,
        })
        .expect("expected ReceivedCall");
    let recv_ok: bool = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::ReceivedCall {
            peer: recv_peer.into(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(recv_ok);
    instance
        .current_call_mut()
        .unwrap()
        .handle_received_call_result(Ok(()), 30_100);
    instance.current_call_mut().unwrap().actually_answer();
    let (acc_peer, acc_gb, acc_proto) = instance
        .current_call_mut()
        .unwrap()
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::AcceptCall {
                peer,
                g_b,
                protocol,
            } => Some((peer, g_b, protocol)),
            _ => None,
        })
        .expect("expected AcceptCall");
    let acc_resp: tl_enums::phone::PhoneCall = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::AcceptCall {
            peer: acc_peer.into(),
            g_b: acc_gb,
            protocol: acc_proto.into(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    instance
        .current_call_mut()
        .unwrap()
        .handle_accept_call_result(Ok(PhoneCallUpdate::try_from(acc_resp).unwrap()), 30_200);
    assert_eq!(
        srv_state.lock().unwrap().dh_requests,
        vec![(0, 256), (14, 256)]
    );
    assert_eq!(srv_state.lock().unwrap().received_call_ids, vec![40_002]);
    assert!(!srv_state.lock().unwrap().accepted_gb.is_empty());

    client.shutdown();
}

// ============================================================================
// 2. Group Call Wire Protocol: Join JSON, Self-Update Flags, Gap Refetch,
//    UpdatesHandler Ordering/Pass-Through, RTMP Stream Channels, Leave Source
// ============================================================================

#[tokio::test]
async fn group_call_join_edit_gap_refetch_rtmp_leave_and_updates_routing_over_mtproto() {
    #[derive(Default)]
    struct GroupServerState {
        join_json: Option<String>,
        self_edits: Vec<(Option<bool>, Option<i32>, Option<bool>)>,
        get_group_call_limits: Vec<i32>,
        get_participants_offsets: Vec<(String, i32)>,
        stream_channels_calls: usize,
        leave_source: Option<i32>,
    }

    let gstate = Arc::new(Mutex::new(GroupServerState::default()));
    let gstate_cb = Arc::clone(&gstate);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::updates::GetState::CONSTRUCTOR_ID => ServerAction::ok(&tl_state(1, 1)),
        c if c == tl_fns::phone::JoinGroupCall::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::JoinGroupCall::from_bytes(&rpc.body).unwrap();
            let tl_enums::DataJSON::DataJSON(dj) = req.params;
            let parsed: serde_json::Value =
                serde_json::from_str(&dj.data).expect("valid join JSON");
            assert_eq!(parsed["ssrc"], 888_777);
            assert_eq!(parsed["ufrag"], "ufr4");
            assert_eq!(parsed["pwd"], "pwd0123456789abcdefghijk");
            assert_eq!(parsed["fingerprints"][0]["hash"], "sha-256");
            assert_eq!(parsed["fingerprints"][0]["setup"], "active");
            gstate_cb.lock().unwrap().join_json = Some(dj.data);
            ServerAction::ok(&tl_empty_updates())
        }
        c if c == tl_fns::phone::EditGroupCallParticipant::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::EditGroupCallParticipant::from_bytes(&rpc.body).unwrap();
            // Verify raw wire flags: bit 1 (`f_volume`) MUST be 0 on self-updates!
            let raw_flags = u32::from_le_bytes(rpc.body[4..8].try_into().unwrap());
            assert_eq!(
                raw_flags & (1 << 1),
                0,
                "Flag::f_volume must NOT be set on wire for self-updates (calls_group_call.cpp:3915-3940)"
            );
            assert!(req.volume.is_none());
            gstate_cb
                .lock()
                .unwrap()
                .self_edits
                .push((req.muted, req.volume, req.raise_hand));
            ServerAction::ok(&tl_empty_updates())
        }
        c if c == tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::GetGroupCall::from_bytes(&rpc.body).unwrap();
            gstate_cb
                .lock()
                .unwrap()
                .get_group_call_limits
                .push(req.limit);
            let resp = tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
                call: tl_group_call_info(80_001, 0xCAFE, 3, 5),
                participants: vec![
                    tl_group_call_participant(100, 888_777, false, false, true),
                    tl_group_call_participant(101, 1001, true, false, false),
                    tl_group_call_participant(102, 1002, true, false, false),
                ],
                participants_next_offset: "next_p2".to_owned(),
                chats: vec![],
                users: vec![],
            });
            ServerAction::ok(&resp)
        }
        c if c == tl_fns::phone::GetGroupParticipants::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::GetGroupParticipants::from_bytes(&rpc.body).unwrap();
            gstate_cb
                .lock()
                .unwrap()
                .get_participants_offsets
                .push((req.offset, req.limit));
            let resp = tl_enums::phone::GroupParticipants::GroupParticipants(
                tl_types::phone::GroupParticipants {
                    count: 5,
                    participants: vec![
                        tl_group_call_participant(103, 1003, true, false, false),
                        tl_group_call_participant(104, 1004, true, false, false),
                    ],
                    next_offset: String::new(),
                    chats: vec![],
                    users: vec![],
                    version: 3,
                },
            );
            ServerAction::ok(&resp)
        }
        c if c == tl_fns::phone::GetGroupCallStreamChannels::CONSTRUCTOR_ID => {
            let mut st = gstate_cb.lock().unwrap();
            st.stream_channels_calls += 1;
            if st.stream_channels_calls == 1 {
                ServerAction::ok(
                    &tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(
                        tl_types::phone::GroupCallStreamChannels {
                            channels: vec![
                                tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(
                                    tl_types::GroupCallStreamChannel {
                                        channel: 1,
                                        scale: 0,
                                        last_timestamp_ms: 1_700_000_999_000,
                                    },
                                ),
                            ],
                        },
                    ),
                )
            } else {
                ServerAction::err(400, "GROUPCALL_JOIN_MISSING")
            }
        }
        c if c == tl_fns::phone::LeaveGroupCall::CONSTRUCTOR_ID => {
            let req = tl_fns::phone::LeaveGroupCall::from_bytes(&rpc.body).unwrap();
            gstate_cb.lock().unwrap().leave_source = Some(req.source);
            ServerAction::ok(&tl_empty_updates())
        }
        other => panic!("unexpected group RPC ctor 0x{other:08x}"),
    });

    let (client, _updates_rx) =
        Client::with_transport(test_client_config(2), Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());

    // A. Verify `UpdatesHandler` while `requesting_difference() == true` (before `initialize`):
    //    `updateGroupCallParticipants` and `updateGroupCallChainBlocks` MUST NOT be dropped, and
    //    `ChainBlocks` MUST be ordered before `Participants` (`api_updates.cpp:305-328, 957-977`).
    let mut session = Session::new(tg_data::UserId(100));
    let mut updates_handler = UpdatesHandler::new();
    let batch_during_diff = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::GroupCallParticipants(tl_types::UpdateGroupCallParticipants {
                call: tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                    id: 80_001,
                    access_hash: 0xCAFE,
                }),
                participants: vec![tl_group_call_participant(101, 1001, true, true, false)],
                version: 2,
            }),
            tl_enums::Update::GroupCallChainBlocks(tl_types::UpdateGroupCallChainBlocks {
                call: tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                    id: 80_001,
                    access_hash: 0xCAFE,
                }),
                sub_chain_id: 0,
                blocks: vec![vec![1, 2, 3]],
                next_offset: 1,
            }),
        ],
        users: vec![],
        chats: vec![],
        date: now_unix_secs(),
        seq: 2,
    });
    updates_handler
        .handle_updates(&rpc, &mut session, batch_during_diff, None, 500)
        .await
        .unwrap();
    let diff_events = updates_handler.drain_events();
    assert_eq!(diff_events.len(), 2);
    assert!(
        matches!(
            &diff_events[0],
            UpdateEvent::CallUpdate(u) if matches!(**u, tl_enums::Update::GroupCallChainBlocks(_))
        ),
        "GroupCallChainBlocks must be delivered first during requesting_difference"
    );
    assert!(
        matches!(
            &diff_events[1],
            UpdateEvent::CallUpdate(u) if matches!(**u, tl_enums::Update::GroupCallParticipants(_))
        ),
        "GroupCallParticipants must be delivered second during requesting_difference"
    );

    // Now initialize updates state cleanly.
    tokio::time::timeout(
        TEST_TIMEOUT,
        updates_handler.initialize(&rpc, &mut session, 1_000),
    )
    .await
    .unwrap()
    .unwrap();

    // B. Join group call with real `GroupJoinPayload` JSON over MTProto.
    let self_peer = PeerId::from_user(UserId(100));
    let call_id = CallId(80_001);
    let mut group = GroupCallSession::new(self_peer, self_peer, call_id, 0xCAFE, false, 0, false);
    let _ = group.take_actions();

    let join_payload = GroupJoinPayload {
        ssrc: 888_777,
        ufrag: "ufr4".to_owned(),
        pwd: "pwd0123456789abcdefghijk".to_owned(),
        fingerprints: vec![DtlsFingerprint {
            hash: "sha-256".to_owned(),
            setup: Some("active".to_owned()),
            fingerprint: "01:23:45:67:89:AB:CD:EF".to_owned(),
        }],
        ssrc_groups: vec![JoinSsrcGroup {
            semantics: "FID".to_owned(),
            sources: vec![888_777, 888_778],
        }],
    };
    group.on_join_payload_ready(join_payload.ssrc, join_payload.to_json().unwrap());
    let (j_call, j_muted, j_vid_stopped, j_params) = group
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            GroupCallAction::JoinGroupCall {
                call,
                muted,
                video_stopped,
                params_json,
                ..
            } => Some((call, muted, video_stopped, params_json)),
            _ => None,
        })
        .expect("expected JoinGroupCall");
    let _: tl_enums::Updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::JoinGroupCall {
            muted: j_muted,
            video_stopped: j_vid_stopped,
            call: j_call.try_into().unwrap(),
            join_as: tl_enums::InputPeer::Self_,
            invite_hash: None,
            public_key: None,
            block: None,
            params: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: j_params }),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    group.handle_join_done(MuteState::Muted, true);
    group.set_instance_connected(true, false);
    assert_eq!(group.state(), GroupCallState::Joined);
    let _ = group.take_actions();

    // C. Self unmute + raise hand over MTProto -> verify NO `f_volume` on wire!
    group.set_muted_and_update(MuteState::Active);
    let edit1 = group
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            GroupCallAction::EditGroupCallParticipant {
                call,
                muted,
                volume,
                raise_hand,
                video_stopped,
                video_paused,
                presentation_paused,
                ..
            } => Some((
                call,
                muted,
                volume,
                raise_hand,
                video_stopped,
                video_paused,
                presentation_paused,
            )),
            _ => None,
        })
        .expect("expected self unmute EditGroupCallParticipant");
    let _: tl_enums::Updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::EditGroupCallParticipant {
            call: edit1.0.try_into().unwrap(),
            participant: tl_enums::InputPeer::Self_,
            muted: edit1.1,
            volume: edit1.2,
            raise_hand: edit1.3,
            video_stopped: edit1.4,
            video_paused: edit1.5,
            presentation_paused: edit1.6,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    group.handle_self_update_done(None);

    group.set_muted(MuteState::ForceMuted);
    let _ = group.take_actions();
    group.set_muted_and_update(MuteState::RaisedHand);
    let edit2 = group
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            GroupCallAction::EditGroupCallParticipant {
                call,
                muted,
                volume,
                raise_hand,
                video_stopped,
                video_paused,
                presentation_paused,
                ..
            } => Some((
                call,
                muted,
                volume,
                raise_hand,
                video_stopped,
                video_paused,
                presentation_paused,
            )),
            _ => None,
        })
        .expect("expected raise_hand EditGroupCallParticipant");
    let _: tl_enums::Updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::EditGroupCallParticipant {
            call: edit2.0.try_into().unwrap(),
            participant: tl_enums::InputPeer::Self_,
            muted: edit2.1,
            volume: edit2.2,
            raise_hand: edit2.3,
            video_stopped: edit2.4,
            video_paused: edit2.5,
            presentation_paused: edit2.6,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    group.handle_self_update_done(None);
    assert_eq!(
        gstate.lock().unwrap().self_edits,
        vec![(Some(false), None, None), (None, None, Some(true))]
    );

    // D. Seed 4 participants at v1, inject version gap (v3 skipping v2) -> timer fires ->
    //    `phone.getGroupCall(limit=3)` -> `phone.getGroupParticipants(offset="next_p2", limit=50)`.
    let initial_parts: Vec<GroupCallParticipantData> = [
        (100, 888_777, true),
        (101, 1001, false),
        (102, 1002, false),
        (103, 1003, false),
    ]
    .into_iter()
    .map(|(uid, ssrc, is_self)| {
        GroupCallParticipantData::from(tl_group_call_participant(uid, ssrc, true, false, is_self))
    })
    .collect();
    group.process_full_call(
        GroupCallObject::from(tl_group_call_info(80_001, 0xCAFE, 1, 4)),
        &initial_parts,
    );
    let _ = group.take_actions();

    group.handle_versioned_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id,
            participants: vec![GroupCallParticipantData::from(tl_group_call_participant(
                104, 1004, true, true, false,
            ))],
            version: 3,
        },
    ));
    let _ = group.take_actions();
    group.on_timer(GroupCallTimerKind::ReloadByQueuedUpdates, 4_000);
    let (r_call, r_limit) = group
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            GroupCallAction::ReloadGroupCall { call, limit } => Some((call, limit)),
            _ => None,
        })
        .expect("expected ReloadGroupCall");
    let raw_full: tl_enums::phone::GroupCall = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::GetGroupCall {
            call: r_call.try_into().unwrap(),
            limit: r_limit,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let full = PhoneGroupCallResponse::from(raw_full);
    group.handle_reload_group_call_result(Ok((
        full.call,
        &full.participants,
        &full.participants_next_offset,
    )));
    let (p_call, p_offset, p_limit) = group
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            GroupCallAction::GetGroupParticipants {
                call,
                offset,
                limit,
            } => Some((call, offset, limit)),
            _ => None,
        })
        .expect("expected GetGroupParticipants");
    let tl_enums::phone::GroupParticipants::GroupParticipants(p_resp) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::GetGroupParticipants {
            call: p_call.try_into().unwrap(),
            ids: vec![],
            sources: vec![],
            offset: p_offset,
            limit: p_limit,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let page2: Vec<GroupCallParticipantData> =
        p_resp.participants.into_iter().map(Into::into).collect();
    group.handle_reload_participants_result(Ok((&page2, p_resp.count)));
    assert_eq!(group.participants_list().participants().len(), 5);
    assert_eq!(gstate.lock().unwrap().get_group_call_limits, vec![3]);
    assert_eq!(
        gstate.lock().unwrap().get_participants_offsets,
        vec![("next_p2".to_owned(), 50)]
    );

    // E. Leave group call -> `phone.leaveGroupCall(source = 888_777)` on wire.
    group.hangup();
    let (l_call, l_source) = group
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            GroupCallAction::LeaveGroupCall { call, source } => Some((call, source)),
            _ => None,
        })
        .expect("expected LeaveGroupCall");
    let _: tl_enums::Updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::LeaveGroupCall {
            call: l_call.try_into().unwrap(),
            source: l_source as i32,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    group.handle_leave_done();
    assert_eq!(group.state(), GroupCallState::Ended);
    assert_eq!(gstate.lock().unwrap().leave_source, Some(888_777));

    // F. RTMP `phone.getGroupCallStreamChannels` over MTProto (success then GROUPCALL_JOIN_MISSING).
    let mut rtmp = GroupCallSession::new(self_peer, self_peer, call_id, 0xCAFE, false, 0, true);
    let _ = rtmp.take_actions();
    rtmp.on_join_payload_ready(999_111, join_payload.to_json().unwrap());
    let _ = rtmp.take_actions();
    rtmp.handle_join_done(MuteState::Muted, true);
    rtmp.set_instance_connected(true, false);
    let _ = rtmp.take_actions();

    rtmp.request_stream_channels();
    let sc_call = rtmp
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            GroupCallAction::GetGroupCallStreamChannels { call, .. } => Some(call),
            _ => None,
        })
        .expect("expected GetGroupCallStreamChannels");
    let sc_resp: tl_enums::phone::GroupCallStreamChannels = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::GetGroupCallStreamChannels {
            call: sc_call.clone().try_into().unwrap(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let decoded = PhoneGroupCallStreamChannelsResponse::from(sc_resp);
    let ts = rtmp.handle_stream_channels_result(Ok(&decoded.channels));
    assert_eq!(ts, 1_700_000_999_000);

    // Second call returns 400 GROUPCALL_JOIN_MISSING -> triggers rejoin!
    rtmp.request_stream_channels();
    let _ = rtmp.take_actions();
    let err = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::phone::GetGroupCallStreamChannels {
            call: sc_call.try_into().unwrap(),
        }),
    )
    .await
    .unwrap()
    .expect_err("expected GROUPCALL_JOIN_MISSING");
    match err {
        InvocationError::Rpc(rpc_err) => {
            assert_eq!(rpc_err.code, 400);
            assert_eq!(rpc_err.name, "GROUPCALL_JOIN_MISSING");
            assert_eq!(rtmp.handle_stream_channels_result(Err(&rpc_err.name)), 0);
        }
        other => panic!("expected InvocationError::Rpc, got {other:?}"),
    }
    assert_eq!(rtmp.state(), GroupCallState::Joining);
    assert!(
        rtmp.take_actions()
            .contains(&GroupCallAction::EmitJoinPayload)
    );

    // Also verify `peer_to_tl` roundtrip compiles and matches self peer.
    assert!(matches!(
        peer_to_tl(self_peer).unwrap(),
        tl_enums::Peer::User(u) if u.user_id == 100
    ));

    client.shutdown();
}

// ============================================================================
// 3. Required Call RPC Errors over Real MTProto 2.0 `rpc_error` Wire Packets
// ============================================================================

#[tokio::test]
async fn call_rpc_errors_decoded_through_mtproto_and_handled_by_call_state_machine() {
    // `FLOOD_WAIT_120` exceeds tdesktop's auto-retry threshold so it surfaces immediately as
    // `InvocationError::Rpc`; `FLOOD_WAIT_1` is below the threshold so MTProto auto-retries it.
    let errors = [
        (400, "CALL_PROTOCOL_LAYER_INVALID"),
        (400, "CALL_ALREADY_DECLINED"),
        (400, "PARTICIPANT_VERSION_OUTDATED"),
        (403, "USER_PRIVACY_RESTRICTED"),
        (420, "FLOOD_WAIT_120"),
    ];
    let idx = Arc::new(AtomicU64::new(0));
    let idx_cb = Arc::clone(&idx);

    let server = ScriptedServer::new(move |_srv, rpc| {
        assert_eq!(rpc.ctor, tl_fns::phone::RequestCall::CONSTRUCTOR_ID);
        let i = idx_cb.fetch_add(1, Ordering::SeqCst) as usize;
        let (code, msg) = errors[i];
        ServerAction::err(code, msg)
    });

    let (client, _updates_rx) =
        Client::with_transport(test_client_config(2), Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());

    let dh_cfg = DhConfig {
        version: 14,
        g: 3,
        p: DH_GOOD_PRIME.to_vec(),
    };

    for (expected_code, expected_msg) in errors {
        let mut call = tg_calls::call::Call::new_outgoing(
            UserId(100),
            UserId(200),
            false,
            ServerCallConfig::default(),
            CallProtocolConfig::default(),
        );
        call.apply_user_confirmation();
        call.start_with_rng(dh_cfg.clone(), &[0x11; 256], 42, |b| b.fill(0x22));
        let (g_a_hash, protocol) = call
            .take_actions()
            .into_iter()
            .find_map(|a| match a {
                CallAction::RequestCall {
                    g_a_hash, protocol, ..
                } => Some((g_a_hash, protocol)),
                _ => None,
            })
            .unwrap();

        let err = tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::phone::RequestCall {
                video: false,
                user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                    user_id: 200,
                    access_hash: 0,
                }),
                random_id: 42,
                g_a_hash: g_a_hash.to_vec(),
                protocol: protocol.into(),
            }),
        )
        .await
        .unwrap()
        .expect_err("expected rpc_error");

        let msg = match err {
            InvocationError::Rpc(rpc_err) => {
                assert_eq!(rpc_err.code, expected_code);
                assert_eq!(rpc_err.name, expected_msg);
                if expected_code == 420 {
                    assert!(rpc_err.is_flood_error());
                    assert_eq!(rpc_err.flood_wait_seconds(), Some(120));
                }
                rpc_err.name
            }
            other => panic!("unexpected InvocationError for {expected_msg}: {other:?}"),
        };

        call.handle_request_call_result(Err(&msg), 1_000);
        let actions = call.take_actions();
        assert!(actions.contains(&CallAction::ShowError(expected_msg.to_owned())));
        call.on_timer(CallTimerKind::FinishByTimeout, 6_000);
        assert_eq!(call.state(), CallState::Failed);
    }

    assert_eq!(server.rpcs().len(), errors.len());
    client.shutdown();
}
