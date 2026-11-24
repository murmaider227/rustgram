//! Byte-level tdesktop conformance tests for Bots, Inline Queries & Mini Apps (WP-54)
//! against an in-process scripted fake Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233):
//! - `Telegram/SourceFiles/apiwrap.cpp` (`ApiWrap::sendBotStart`, `messages.startBot`)
//! - `Telegram/SourceFiles/api/api_bot.cpp` (`SendBotCallbackData`, `ActivateBotButton`)
//! - `Telegram/SourceFiles/boxes/url_auth_box.cpp` (`requestUrlAuth`, `acceptUrlAuth`,
//!   `declineUrlAuth`, `checkUrlAuthMatchCode`)
//! - `Telegram/SourceFiles/inline_bots/inline_results_widget.cpp` (`getInlineBotResults`,
//!   `sendInlineBotResult`, debounce/pagination/switch_pm/switch_webview)
//! - `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp` (`getBotApp`, `requestWebView`,
//!   `requestSimpleWebView`, `requestMainWebView`, `requestAppWebView`, `requestChatJoinWebView`,
//!   `prolongWebView`, `getPreparedInlineMessage`, `bots.*` permission/discovery RPCs)
//! - `Telegram/SourceFiles/api/api_chat_participants.cpp` (`bots.getBotRecommendations`)
//! - `Telegram/SourceFiles/window/themes/window_theme.cpp` (`WebViewParams`)
//!
//! Security & test-hygiene notes:
//! - Zero network access: every connection runs over an in-process `TransportFactory`.
//! - Uses synthetic credentials (`api_id = 1`, `api_hash = "0000...0000"`), synthetic peer/bot IDs,
//!   fixture-only URLs (`https://fixture.invalid/...`), and an ephemeral test-only RSA keypair.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for, message_from_tl};
use tg_api::{ApiCredentials, InvocationError, RpcClient};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_data::{ChatId, FullMsgId, MsgId, PeerId, Session, UserId};
use tg_mtproto::dc_options::DcId;
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, PacketConnection, TransportFactory,
};
use tg_session::{MtprotoRpc, build_mtproto_client_config};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, RawObject,
    Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::mpsc;

// ============================================================================
// Test-only constants & byte-level scripted MTProto 2.0 fake server
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

/// Exact 15 theme parameter keys produced by tdesktop's `Window::Theme::WebViewParams()`
/// (`Telegram/SourceFiles/window/themes/window_theme.cpp:1637-1693`).
const TDESKTOP_WEBVIEW_THEME_KEYS: &[&str] = &[
    "bg_color",
    "secondary_bg_color",
    "text_color",
    "hint_color",
    "link_color",
    "button_color",
    "button_text_color",
    "header_bg_color",
    "accent_text_color",
    "section_bg_color",
    "section_header_text_color",
    "subtitle_text_color",
    "destructive_text_color",
    "bottom_bar_bg_color",
    "section_separator_color",
];

fn sample_theme_params_json() -> String {
    serde_json::json!({
        "bg_color": "#ffffff",
        "secondary_bg_color": "#f1f1f1",
        "text_color": "#000000",
        "hint_color": "#999999",
        "link_color": "#168acd",
        "button_color": "#40a7e3",
        "button_text_color": "#ffffff",
        "header_bg_color": "#ffffff",
        "accent_text_color": "#168acd",
        "section_bg_color": "#ffffff",
        "section_header_text_color": "#168acd",
        "subtitle_text_color": "#999999",
        "destructive_text_color": "#d14e4e",
        "bottom_bar_bg_color": "#ffffff",
        "section_separator_color": "#e5e5e5"
    })
    .to_string()
}

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
    PublicKey::from_components(&hex_decode(TEST_RSA_N_HEX), &[0x01, 0x00, 0x01])
        .expect("valid test RSA public key")
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
struct RpcRecord {
    #[allow(dead_code)]
    dc_id: DcId,
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct IncomingRpc {
    #[allow(dead_code)]
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

    fn bool_reply(v: bool) -> Self {
        let ctor = if v { BOOL_TRUE_ID } else { BOOL_FALSE_ID };
        Self::Reply(ctor.to_le_bytes().to_vec())
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
    #[allow(dead_code)]
    dc_id: DcId,
    #[allow(dead_code)]
    tx: mpsc::UnboundedSender<tl_enums::Updates>,
}

struct ScriptedServerInner {
    rsa_pub: PublicKey,
    rsa_n: Vec<u8>,
    rsa_d: Vec<u8>,
    next_conn_id: AtomicU64,
    dh_counter: AtomicU64,
    keys: Mutex<HashMap<u64, ServerKeyEntry>>,
    rpcs: Mutex<Vec<RpcRecord>>,
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

    fn rpcs(&self) -> Vec<RpcRecord> {
        self.inner.rpcs.lock().unwrap().clone()
    }

    fn rpcs_of<F: Identifiable>(&self) -> Vec<RpcRecord> {
        self.rpcs()
            .into_iter()
            .filter(|r| r.ctor == F::CONSTRUCTOR_ID)
            .collect()
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
        let mut data_with_padding = data_with_hash[0..192].to_vec();
        data_with_padding.reverse();
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
                        let Some(raw) = maybe_raw else {
                            break;
                        };
                        if raw.len() < 8 {
                            break;
                        }
                        let auth_key_id = u64::from_le_bytes(raw[0..8].try_into().unwrap());
                        if auth_key_id == 0 {
                            let msg_len =
                                u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
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
                                )
                                .to_bytes();
                                let digest = sha1(&[&inner]);
                                let mut answer_with_hash = Vec::with_capacity(20 + inner.len() + 16);
                                answer_with_hash.extend_from_slice(&digest);
                                answer_with_hash.extend_from_slice(&inner);
                                while answer_with_hash.len() % 16 != 0 {
                                    answer_with_hash.push(0);
                                }
                                let (aes_key, aes_iv) =
                                    dh_server_aes_key_iv(&new_nonce, &req.server_nonce);
                                let encrypted_answer =
                                    aes_ige_encrypt(&answer_with_hash, &aes_key, &aes_iv);
                                let resp = mtp_enums::ServerDHParams::Ok(
                                    mtp_types::ServerDHParamsOk {
                                        nonce: req.nonce,
                                        server_nonce: req.server_nonce,
                                        encrypted_answer,
                                    },
                                );
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
                                let dec = aes_ige_decrypt(
                                    &req.encrypted_data,
                                    &pending.aes_key,
                                    &pending.aes_iv,
                                );
                                let mut dec_cur = Cursor::new(&dec[20..]);
                                let mtp_enums::ClientDHInnerData::ClientDHInnerData(cdh) =
                                    mtp_enums::ClientDHInnerData::deserialize(&mut dec_cur).unwrap();
                                let shared = pad_be_256(
                                    &mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME).unwrap(),
                                );
                                let auth_key = AuthKey::new(shared);
                                let nn_lo = u64::from_le_bytes(pending.new_nonce[0..8].try_into().unwrap());
                                let sn_lo = u64::from_le_bytes(pending.server_nonce[0..8].try_into().unwrap());
                                let server_salt = nn_lo ^ sn_lo;
                                let nh1 = dh_new_nonce_hash(
                                    &pending.new_nonce,
                                    1,
                                    &auth_key.aux_hash(),
                                );
                                server.inner.keys.lock().unwrap().insert(
                                    auth_key.key_id_u64(),
                                    ServerKeyEntry {
                                        key: auth_key,
                                        server_salt,
                                    },
                                );
                                let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(
                                    mtp_types::DhGenOk {
                                        nonce: pending.nonce,
                                        server_nonce: pending.server_nonce,
                                        new_nonce_hash1: nh1,
                                    },
                                );
                                let out_id = next_server_msg_id(&mut server_msg_counter, true);
                                let _ = peer.tx.send(Ok(encode_unencrypted_packet(out_id, &ok.to_bytes())));
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
                            .expect("decrypt client MTProto 2.0 packet");
                        let session_id = dec.session_id;
                        active_key = Some((entry.key.clone(), entry.server_salt, session_id));

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
                                let ping =
                                    mtp_functions::PingDelayDisconnect::from_bytes(&body).unwrap();
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
                                continue;
                            }
                            if outer_ctor == tl_fns::auth::BindTempAuthKey::CONSTRUCTOR_ID {
                                let rpc_res = Boxed(RpcResult {
                                    req_msg_id: msg_id as i64,
                                    result: RawObject(BOOL_TRUE_ID.to_le_bytes().to_vec()),
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
                                continue;
                            }

                            let inner_body = unwrap_init_layer(&body);
                            let inner_ctor =
                                u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                            server.inner.rpcs.lock().unwrap().push(RpcRecord {
                                dc_id,
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

                            match action {
                                ServerAction::Reply(payload) => {
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
                                ServerAction::RpcError(code, message) => {
                                    let err_bytes =
                                        mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                            error_code: code,
                                            error_message: message,
                                        })
                                        .to_bytes();
                                    let rpc_res = Boxed(RpcResult {
                                        req_msg_id: msg_id as i64,
                                        result: RawObject(err_bytes),
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
                        }
                    }
                }
            }
            server.inner.live_pushers.lock().unwrap().remove(&conn_id);
        });
    }
}

fn unwrap_init_layer(raw: &[u8]) -> Vec<u8> {
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

fn make_mtproto_rpc(server: &ScriptedServer) -> (MtprotoRpc, Client) {
    let creds = ApiCredentials {
        api_id: FAKE_API_ID,
        api_hash: FAKE_API_HASH.to_owned(),
    };
    let mut cfg: ClientConfig =
        build_mtproto_client_config(&creds, 2, tg_core::Environment::Production, None);
    cfg.use_pfs = false;
    let (client, _updates_rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&client);
    let rpc = MtprotoRpc::new(client.clone());
    (rpc, client)
}

fn synthetic_bot_user(id: i64, username: &str, name: &str, has_main_app: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: true,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: true,
        premium: false,
        attach_menu_enabled: true,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: has_main_app,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 7),
        first_name: Some(name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: Some(1),
        restriction_reason: None,
        bot_inline_placeholder: Some("Search items...".to_owned()),
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: Some(1200),
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

fn synthetic_basic_chat(id: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Chat(tl_types::Chat {
        creator: true,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 5,
        date: 1_700_000_000,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    })
}

fn synthetic_message_updates(msg_id: i32, peer: tl_enums::Peer, text: &str) -> tl_enums::Updates {
    let msg = tl_enums::Message::Message(tl_types::Message {
        out: true,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id: msg_id,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_700_000_100,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    });
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: msg,
            pts: 10,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_100,
        seq: 1,
    })
}

fn seed_session() -> Session {
    let mut session = Session::new(UserId(100));
    apply_tl_users_and_chats(
        &mut session,
        &[synthetic_bot_user(500, "fixture_bot", "Fixture Bot", true)],
        &[synthetic_basic_chat(800, "Fixture Group")],
    );
    session
}

// ============================================================================
// 1. messages.startBot (1:1 ?start= vs group ?startgroup= vs plain /start)
// ============================================================================

#[tokio::test]
async fn start_bot_wire_encoding_private_and_group_flows() {
    let server = ScriptedServer::new(|_srv, rpc| {
        if rpc.ctor == tl_fns::messages::StartBot::CONSTRUCTOR_ID {
            let req = tl_fns::messages::StartBot::from_bytes(&rpc.body)
                .expect("decode messages.startBot");
            let peer = match &req.peer {
                tl_enums::InputPeer::Empty => {
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: 500 })
                }
                tl_enums::InputPeer::Chat(c) => {
                    tl_enums::Peer::Chat(tl_types::PeerChat { chat_id: c.chat_id })
                }
                other => panic!("unexpected peer in startBot: {other:?}"),
            };
            ServerAction::ok(&synthetic_message_updates(
                101,
                peer,
                &format!("/start {}", req.start_param),
            ))
        } else {
            ServerAction::err(400, "UNEXPECTED_RPC")
        }
    });

    let (rpc, _client) = make_mtproto_rpc(&server);
    let mut session = seed_session();
    let bot_input = input_user_for(&session, UserId(500));
    let group_input = input_peer_for(&session, PeerId::from_chat(ChatId(800)));

    // 1:1 `?start=ref_alpha_1`: tdesktop `apiwrap.cpp:5128` sends `peer = InputPeer::Empty`.
    let dm_updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::StartBot {
            bot: bot_input.clone(),
            peer: tl_enums::InputPeer::Empty,
            random_id: 0x1111_2222_3333_4444,
            start_param: "ref_alpha_1".to_owned(),
        }),
    )
    .await
    .expect("timeout")
    .expect("1:1 startBot ok");

    if let tl_enums::Updates::Updates(u) = &dm_updates
        && let Some(tl_enums::Update::NewMessage(nm)) = u.updates.first()
        && let Some(msg) = message_from_tl(&nm.message, UserId(100))
    {
        session.insert_message(msg);
    }
    assert!(
        session
            .message(FullMsgId::new(PeerId::from_user(UserId(500)), MsgId(101)))
            .is_some()
    );

    // Group `?startgroup=welcome_grp`: tdesktop `apiwrap.cpp:5128` sends `peer = chat->input()`.
    let _grp_updates = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::StartBot {
            bot: bot_input,
            peer: group_input,
            random_id: 0x5555_6666_7777_8888,
            start_param: "welcome_grp".to_owned(),
        }),
    )
    .await
    .expect("timeout")
    .expect("group startBot ok");

    // Verify exact wire constructor & zero plain-text SendMessage substitution.
    assert!(
        server.rpcs_of::<tl_fns::messages::SendMessage>().is_empty(),
        "messages.startBot must NOT substitute plain-text messages.sendMessage"
    );
    let starts = server.rpcs_of::<tl_fns::messages::StartBot>();
    assert_eq!(starts.len(), 2);

    let first = tl_fns::messages::StartBot::from_bytes(&starts[0].body).unwrap();
    assert_eq!(first.peer, tl_enums::InputPeer::Empty);
    assert_eq!(first.start_param, "ref_alpha_1");
    assert_eq!(first.random_id, 0x1111_2222_3333_4444);

    let second = tl_fns::messages::StartBot::from_bytes(&starts[1].body).unwrap();
    assert!(matches!(second.peer, tl_enums::InputPeer::Chat(c) if c.chat_id == 800));
    assert_eq!(second.start_param, "welcome_grp");
    assert_eq!(second.random_id, 0x5555_6666_7777_8888);
}

// ============================================================================
// 2. Inline Bot Query / Pagination / SwitchPM / SwitchWebView & SendInlineBotResult
// ============================================================================

#[tokio::test]
async fn inline_bot_query_pagination_switch_pm_webview_and_send() {
    let calls = Arc::new(AtomicU32::new(0));
    let calls_cl = Arc::clone(&calls);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetInlineBotResults::from_bytes(&rpc.body).unwrap();
            let n = calls_cl.fetch_add(1, Ordering::SeqCst);
            if req.query == "disabled" {
                return ServerAction::err(400, "BOT_INLINE_DISABLED");
            }
            if req.offset.is_empty() && n == 0 {
                ServerAction::ok(&tl_enums::messages::BotResults::BotResults(
                    tl_types::messages::BotResults {
                        gallery: true,
                        query_id: 9001,
                        next_offset: Some("page_2".to_owned()),
                        switch_pm: Some(tl_enums::InlineBotSwitchPM::InlineBotSwitchPM(
                            tl_types::InlineBotSwitchPM {
                                text: "Configure Bot".to_owned(),
                                start_param: "inline_cfg".to_owned(),
                            },
                        )),
                        switch_webview: None,
                        results: vec![tl_enums::BotInlineResult::BotInlineResult(
                            tl_types::BotInlineResult {
                                id: "r1".to_owned(),
                                r#type: "article".to_owned(),
                                title: Some("First Result".to_owned()),
                                description: Some("Page 1".to_owned()),
                                url: Some("https://fixture.invalid/r1".to_owned()),
                                thumb: None,
                                content: None,
                                send_message: tl_enums::BotInlineMessage::Text(
                                    tl_types::BotInlineMessageText {
                                        no_webpage: false,
                                        invert_media: false,
                                        message: "Picked r1".to_owned(),
                                        entities: None,
                                        reply_markup: None,
                                    },
                                ),
                            },
                        )],
                        cache_time: 120,
                        users: vec![synthetic_bot_user(500, "fixture_bot", "Fixture Bot", true)],
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::messages::BotResults::BotResults(
                    tl_types::messages::BotResults {
                        gallery: false,
                        query_id: 9002,
                        next_offset: None,
                        switch_pm: None,
                        switch_webview: Some(tl_enums::InlineBotWebView::InlineBotWebView(
                            tl_types::InlineBotWebView {
                                text: "Open Mini App".to_owned(),
                                url: "https://fixture.invalid/switch-wv".to_owned(),
                            },
                        )),
                        results: vec![tl_enums::BotInlineResult::BotInlineResult(
                            tl_types::BotInlineResult {
                                id: "r2".to_owned(),
                                r#type: "article".to_owned(),
                                title: Some("Second Result".to_owned()),
                                description: Some("Page 2".to_owned()),
                                url: None,
                                thumb: None,
                                content: None,
                                send_message: tl_enums::BotInlineMessage::Text(
                                    tl_types::BotInlineMessageText {
                                        no_webpage: true,
                                        invert_media: false,
                                        message: "Picked r2".to_owned(),
                                        entities: None,
                                        reply_markup: None,
                                    },
                                ),
                            },
                        )],
                        cache_time: 60,
                        users: Vec::new(),
                    },
                ))
            }
        }
        c if c == tl_fns::messages::SendInlineBotResult::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::SendInlineBotResult::from_bytes(&rpc.body).unwrap();
            assert!(req.clear_draft);
            assert_eq!(req.query_id, 9001);
            assert_eq!(req.id, "r1");
            ServerAction::ok(&synthetic_message_updates(
                201,
                tl_enums::Peer::Chat(tl_types::PeerChat { chat_id: 800 }),
                "Picked r1",
            ))
        }
        _ => ServerAction::err(400, "UNEXPECTED_RPC"),
    });

    let (rpc, _client) = make_mtproto_rpc(&server);
    let session = seed_session();
    let bot = input_user_for(&session, UserId(500));
    let peer = input_peer_for(&session, PeerId::from_chat(ChatId(800)));

    // Page 1: initial query with empty offset, verify switch_pm + next_offset + cache_time.
    let tl_enums::messages::BotResults::BotResults(page1) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetInlineBotResults {
            bot: bot.clone(),
            peer: peer.clone(),
            geo_point: None,
            query: "widgets".to_owned(),
            offset: String::new(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(page1.gallery);
    assert_eq!(page1.query_id, 9001);
    assert_eq!(page1.next_offset.as_deref(), Some("page_2"));
    assert_eq!(page1.cache_time, 120);
    assert!(page1.switch_pm.is_some());

    // Page 2: paginate using `next_offset`, verify switch_webview + exhausted next_offset.
    let tl_enums::messages::BotResults::BotResults(page2) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetInlineBotResults {
            bot: bot.clone(),
            peer: peer.clone(),
            geo_point: None,
            query: "widgets".to_owned(),
            offset: "page_2".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(page2.next_offset, None);
    assert!(page2.switch_webview.is_some());

    // Send chosen inline result with `clear_draft = true`.
    let _sent = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::SendInlineBotResult {
            silent: false,
            background: false,
            clear_draft: true,
            hide_via: false,
            peer: peer.clone(),
            reply_to: None,
            random_id: 77_001,
            query_id: page1.query_id,
            id: "r1".to_owned(),
            schedule_date: None,
            send_as: None,
            quick_reply_shortcut: None,
            allow_paid_stars: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // Error handling: BOT_INLINE_DISABLED surfaces cleanly.
    let err = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetInlineBotResults {
            bot,
            peer,
            geo_point: None,
            query: "disabled".to_owned(),
            offset: String::new(),
        }),
    )
    .await
    .unwrap()
    .expect_err("expected BOT_INLINE_DISABLED");
    assert!(matches!(
        err,
        InvocationError::Rpc(ref e) if e.name == "BOT_INLINE_DISABLED"
    ));
}

// ============================================================================
// 3. Callback Query Answers (data vs game vs password & error handling)
// ============================================================================

#[tokio::test]
async fn callback_answer_flags_cache_time_and_errors() {
    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        c if c == tl_fns::messages::GetBotCallbackAnswer::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetBotCallbackAnswer::from_bytes(&rpc.body).unwrap();
            if req.msg_id == 999 {
                return ServerAction::err(400, "BOT_RESPONSE_TIMEOUT");
            }
            if req.game {
                assert!(
                    req.data.is_none(),
                    "api_bot.cpp:80-86: game callback must omit data"
                );
                ServerAction::ok(&tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(
                    tl_types::messages::BotCallbackAnswer {
                        alert: false,
                        has_url: true,
                        native_ui: false,
                        message: None,
                        url: Some("https://fixture.invalid/game/play".to_owned()),
                        cache_time: 0,
                    },
                ))
            } else if req.password.is_some() {
                ServerAction::ok(&tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(
                    tl_types::messages::BotCallbackAnswer {
                        alert: false,
                        has_url: false,
                        native_ui: false,
                        message: Some("2FA confirmed".to_owned()),
                        url: None,
                        cache_time: 0,
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(
                    tl_types::messages::BotCallbackAnswer {
                        alert: true,
                        has_url: false,
                        native_ui: false,
                        message: Some("Alert body".to_owned()),
                        url: None,
                        cache_time: 45,
                    },
                ))
            }
        }
        _ => ServerAction::err(400, "UNEXPECTED_RPC"),
    });

    let (rpc, _client) = make_mtproto_rpc(&server);
    let session = seed_session();
    let peer = input_peer_for(&session, PeerId::from_user(UserId(500)));

    // 1. Standard data callback -> alert + cache_time
    let tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(ans1) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetBotCallbackAnswer {
            game: false,
            peer: peer.clone(),
            msg_id: 10,
            data: Some(b"act:1".to_vec()),
            password: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ans1.alert);
    assert_eq!(ans1.message.as_deref(), Some("Alert body"));
    assert_eq!(ans1.cache_time, 45);

    // 2. Game callback -> f_game set, data omitted
    let tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(ans2) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetBotCallbackAnswer {
            game: true,
            peer: peer.clone(),
            msg_id: 11,
            data: None,
            password: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ans2.has_url);
    assert_eq!(
        ans2.url.as_deref(),
        Some("https://fixture.invalid/game/play")
    );

    // 3. Password callback -> f_password set
    let tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(ans3) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetBotCallbackAnswer {
            game: false,
            peer: peer.clone(),
            msg_id: 12,
            data: Some(b"withdraw".to_vec()),
            password: Some(tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ans3.message.as_deref(), Some("2FA confirmed"));

    // 4. Bot response timeout error
    let err = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetBotCallbackAnswer {
            game: false,
            peer,
            msg_id: 999,
            data: Some(b"slow".to_vec()),
            password: None,
        }),
    )
    .await
    .unwrap()
    .expect_err("expected BOT_RESPONSE_TIMEOUT");
    assert!(matches!(
        err,
        InvocationError::Rpc(ref e) if e.name == "BOT_RESPONSE_TIMEOUT"
    ));
}

// ============================================================================
// 4. URL Auth: requestUrlAuth, checkUrlAuthMatchCode, acceptUrlAuth, declineUrlAuth
// ============================================================================

#[tokio::test]
async fn url_auth_full_lifecycle_including_check_match_code() {
    let server = ScriptedServer::new(|_srv, rpc| match rpc.ctor {
        c if c == tl_fns::messages::RequestUrlAuth::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::RequestUrlAuth::from_bytes(&rpc.body).unwrap();
            assert!(req.url.is_some());
            assert!(req.peer.is_none());
            ServerAction::ok(&tl_enums::UrlAuthResult::Request(
                tl_types::UrlAuthResultRequest {
                    request_write_access: true,
                    request_phone_number: true,
                    match_codes_first: true,
                    is_app: false,
                    request_wallet: false,
                    bot: synthetic_bot_user(500, "fixture_bot", "Fixture Bot", false),
                    domain: "fixture.invalid".to_owned(),
                    verified_app_name: None,
                    user_id_hint: Some(100),
                    match_codes: Some(vec!["🚀".to_owned(), "🦊".to_owned(), "🎯".to_owned()]),
                    browser: Some("FixtureBrowser".to_owned()),
                    platform: Some("Windows".to_owned()),
                    ip: Some("192.0.2.1".to_owned()),
                    region: Some("Testland".to_owned()),
                    tc_session_id: None,
                    tc_client_id: None,
                },
            ))
        }
        c if c == tl_fns::messages::CheckUrlAuthMatchCode::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::CheckUrlAuthMatchCode::from_bytes(&rpc.body).unwrap();
            if req.url.contains("expired") {
                ServerAction::err(400, "URL_EXPIRED")
            } else {
                ServerAction::bool_reply(req.match_code == "🚀")
            }
        }
        c if c == tl_fns::messages::AcceptUrlAuth::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::AcceptUrlAuth::from_bytes(&rpc.body).unwrap();
            assert!(req.write_allowed);
            assert!(req.share_phone_number);
            assert_eq!(req.match_code.as_deref(), Some("🚀"));
            ServerAction::ok(&tl_enums::UrlAuthResult::Accepted(
                tl_types::UrlAuthResultAccepted {
                    url: Some("https://fixture.invalid/login?tgAuthResult=ok".to_owned()),
                },
            ))
        }
        c if c == tl_fns::messages::DeclineUrlAuth::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::DeclineUrlAuth::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.url, "https://fixture.invalid/decline");
            ServerAction::bool_reply(true)
        }
        _ => ServerAction::err(400, "UNEXPECTED_RPC"),
    });

    let (rpc, _client) = make_mtproto_rpc(&server);
    let url = "https://fixture.invalid/oauth".to_owned();

    // 1. RequestUrlAuth
    let req_res = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::RequestUrlAuth {
            peer: None,
            msg_id: None,
            button_id: None,
            url: Some(url.clone()),
            in_app_origin: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    match req_res {
        tl_enums::UrlAuthResult::Request(r) => {
            assert!(r.match_codes_first);
            assert_eq!(r.match_codes.unwrap().len(), 3);
        }
        other => panic!("expected Request, got {other:?}"),
    }

    // 2. CheckUrlAuthMatchCode: wrong code -> false, right code -> true, expired -> URL_EXPIRED
    let wrong = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::CheckUrlAuthMatchCode {
            url: url.clone(),
            match_code: "🦊".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!wrong);

    let right = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::CheckUrlAuthMatchCode {
            url: url.clone(),
            match_code: "🚀".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(right);

    let expired = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::CheckUrlAuthMatchCode {
            url: "https://fixture.invalid/expired".to_owned(),
            match_code: "🚀".to_owned(),
        }),
    )
    .await
    .unwrap()
    .expect_err("expected URL_EXPIRED");
    assert!(matches!(expired, InvocationError::Rpc(ref e) if e.name == "URL_EXPIRED"));

    // 3. AcceptUrlAuth with match_code + write_allowed + share_phone_number
    let accepted = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::AcceptUrlAuth {
            write_allowed: true,
            share_phone_number: true,
            peer: None,
            msg_id: None,
            button_id: None,
            url: Some(url),
            match_code: Some("🚀".to_owned()),
            tonconnect_session: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        accepted,
        tl_enums::UrlAuthResult::Accepted(a)
            if a.url.as_deref() == Some("https://fixture.invalid/login?tgAuthResult=ok")
    ));

    // 4. DeclineUrlAuth
    let declined = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::DeclineUrlAuth {
            url: "https://fixture.invalid/decline".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(declined);
}

// ============================================================================
// 5. Mini Apps: getBotApp, request*WebView, requestChatJoinWebView, theme_params, prolongWebView
// ============================================================================

#[tokio::test]
async fn mini_app_get_bot_app_all_request_variants_theme_params_and_prolong() {
    let prolong_count = Arc::new(AtomicU32::new(0));
    let prolong_cl = Arc::clone(&prolong_count);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::messages::GetBotApp::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetBotApp::from_bytes(&rpc.body).unwrap();
            if req.hash == 777_888 {
                ServerAction::ok(&tl_enums::messages::BotApp::BotApp(
                    tl_types::messages::BotApp {
                        inactive: false,
                        request_write_access: false,
                        has_settings: true,
                        app: tl_enums::BotApp::NotModified,
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::messages::BotApp::BotApp(
                    tl_types::messages::BotApp {
                        inactive: true,
                        request_write_access: true,
                        has_settings: true,
                        app: tl_enums::BotApp::BotApp(tl_types::BotApp {
                            id: 42_000,
                            access_hash: 999_111,
                            short_name: "catalog".to_owned(),
                            title: "Fixture Catalog".to_owned(),
                            description: "Offline fixture Mini App".to_owned(),
                            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
                            document: None,
                            hash: 777_888,
                        }),
                    },
                ))
            }
        }
        c if c == tl_fns::messages::RequestWebView::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::RequestWebView::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.platform, "tdesktop");
            let Some(tl_enums::DataJSON::DataJSON(ref tp)) = req.theme_params else {
                panic!("missing theme_params");
            };
            let parsed: serde_json::Value = serde_json::from_str(&tp.data).unwrap();
            for key in TDESKTOP_WEBVIEW_THEME_KEYS {
                let v = parsed.get(*key).and_then(|v| v.as_str()).unwrap_or("");
                assert!(
                    v.starts_with('#') && v.len() == 7,
                    "WEBVIEW_THEME_PARAMS key '{key}' missing or invalid hex: {v:?}"
                );
            }
            ServerAction::ok(&tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
                fullsize: true,
                fullscreen: req.fullscreen,
                same_origin: true,
                query_id: Some(55_001),
                url: "https://fixture.invalid/miniapp/webview".to_owned(),
            }))
        }
        c if c == tl_fns::messages::RequestMainWebView::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::RequestMainWebView::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.platform, "tdesktop");
            assert!(req.compact);
            ServerAction::ok(&tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
                fullsize: false,
                fullscreen: false,
                same_origin: true,
                query_id: Some(55_002),
                url: "https://fixture.invalid/miniapp/main".to_owned(),
            }))
        }
        c if c == tl_fns::messages::RequestAppWebView::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::RequestAppWebView::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.platform, "tdesktop");
            assert!(req.write_allowed);
            assert!(matches!(
                req.app,
                tl_enums::InputBotApp::ID(id) if id.id == 42_000 && id.access_hash == 999_111
            ));
            ServerAction::ok(&tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
                fullsize: true,
                fullscreen: true,
                same_origin: true,
                query_id: Some(55_003),
                url: "https://fixture.invalid/miniapp/app".to_owned(),
            }))
        }
        c if c == tl_fns::messages::RequestChatJoinWebView::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::RequestChatJoinWebView::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.platform, "tdesktop");
            assert_eq!(req.query_id, 88_123);
            assert!(req.theme_params.is_some());
            ServerAction::ok(&tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
                fullsize: false,
                fullscreen: false,
                same_origin: true,
                query_id: Some(88_123),
                url: "https://fixture.invalid/miniapp/join-verify".to_owned(),
            }))
        }
        c if c == tl_fns::messages::ProlongWebView::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::ProlongWebView::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.query_id, 55_001);
            assert!(req.silent);
            let n = prolong_cl.fetch_add(1, Ordering::SeqCst);
            // First keep-alive succeeds; second returns false to signal end of session.
            ServerAction::bool_reply(n == 0)
        }
        _ => ServerAction::err(400, "UNEXPECTED_RPC"),
    });

    let (rpc, _client) = make_mtproto_rpc(&server);
    let session = seed_session();
    let bot = input_user_for(&session, UserId(500));
    let peer = input_peer_for(&session, PeerId::from_user(UserId(500)));
    let tp = tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
        data: sample_theme_params_json(),
    });

    // 1. messages.getBotApp (initial hash=0 -> BotApp, second hash=777_888 -> NotModified)
    let tl_enums::messages::BotApp::BotApp(first_app) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetBotApp {
            app: tl_enums::InputBotApp::ShortName(tl_types::InputBotAppShortName {
                bot_id: bot.clone(),
                short_name: "catalog".to_owned(),
            }),
            hash: 0,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(first_app.inactive && first_app.request_write_access && first_app.has_settings);
    let (app_id, app_access_hash, app_hash) = match first_app.app {
        tl_enums::BotApp::BotApp(a) => (a.id, a.access_hash, a.hash),
        tl_enums::BotApp::NotModified => panic!("expected full BotApp"),
    };

    let tl_enums::messages::BotApp::BotApp(cached_app) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetBotApp {
            app: tl_enums::InputBotApp::ShortName(tl_types::InputBotAppShortName {
                bot_id: bot.clone(),
                short_name: "catalog".to_owned(),
            }),
            hash: app_hash,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(cached_app.app, tl_enums::BotApp::NotModified));

    // 2. messages.requestWebView (with all 15 WEBVIEW_THEME_PARAMS keys)
    let tl_enums::WebViewResult::Url(wv) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::RequestWebView {
            from_bot_menu: true,
            silent: true,
            compact: false,
            fullscreen: true,
            peer: peer.clone(),
            bot: bot.clone(),
            url: Some("https://fixture.invalid/button".to_owned()),
            start_param: Some("sp1".to_owned()),
            theme_params: Some(tp.clone()),
            platform: "tdesktop".to_owned(),
            reply_to: None,
            send_as: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wv.query_id, Some(55_001));
    assert!(wv.fullscreen);

    // 3. messages.requestMainWebView
    let tl_enums::WebViewResult::Url(main_wv) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::RequestMainWebView {
            compact: true,
            fullscreen: false,
            peer: peer.clone(),
            bot: bot.clone(),
            start_param: Some("main_tok".to_owned()),
            theme_params: Some(tp.clone()),
            platform: "tdesktop".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(main_wv.url, "https://fixture.invalid/miniapp/main");

    // 4. messages.requestAppWebView
    let tl_enums::WebViewResult::Url(app_wv) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::RequestAppWebView {
            write_allowed: true,
            compact: false,
            fullscreen: true,
            peer: peer.clone(),
            app: tl_enums::InputBotApp::ID(tl_types::InputBotAppID {
                id: app_id,
                access_hash: app_access_hash,
            }),
            start_param: Some("item_7".to_owned()),
            theme_params: Some(tp.clone()),
            platform: "tdesktop".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(app_wv.url, "https://fixture.invalid/miniapp/app");

    // 5. messages.requestChatJoinWebView
    let tl_enums::WebViewResult::Url(join_wv) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::RequestChatJoinWebView {
            query_id: 88_123,
            theme_params: Some(tp),
            platform: "tdesktop".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(join_wv.url, "https://fixture.invalid/miniapp/join-verify");

    // 6. messages.prolongWebView lifecycle (true -> continues, false -> terminates)
    let p1 = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::ProlongWebView {
            silent: true,
            peer: peer.clone(),
            bot: bot.clone(),
            query_id: 55_001,
            reply_to: None,
            send_as: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(p1);

    let p2 = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::ProlongWebView {
            silent: true,
            peer,
            bot,
            query_id: 55_001,
            reply_to: None,
            send_as: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!p2);
    assert_eq!(prolong_count.load(Ordering::SeqCst), 2);
}

// ============================================================================
// 6. All 7 bots.* Permission/Discovery RPCs + getPreparedInlineMessage
// ============================================================================

#[tokio::test]
async fn bots_permissions_discovery_and_prepared_inline_message_conformance() {
    let write_allowed = Arc::new(AtomicU32::new(0));
    let write_cl = Arc::clone(&write_allowed);

    let server = ScriptedServer::new(move |_srv, rpc| match rpc.ctor {
        c if c == tl_fns::bots::CanSendMessage::CONSTRUCTOR_ID => {
            let _req = tl_fns::bots::CanSendMessage::from_bytes(&rpc.body).unwrap();
            ServerAction::bool_reply(write_cl.load(Ordering::SeqCst) > 0)
        }
        c if c == tl_fns::bots::AllowSendMessage::CONSTRUCTOR_ID => {
            let _req = tl_fns::bots::AllowSendMessage::from_bytes(&rpc.body).unwrap();
            write_cl.store(1, Ordering::SeqCst);
            ServerAction::ok(&synthetic_message_updates(
                301,
                tl_enums::Peer::User(tl_types::PeerUser { user_id: 500 }),
                "Bot allowed to write",
            ))
        }
        c if c == tl_fns::bots::CheckDownloadFileParams::CONSTRUCTOR_ID => {
            let req = tl_fns::bots::CheckDownloadFileParams::from_bytes(&rpc.body).unwrap();
            let ok = !req.file_name.contains("..") && req.url.starts_with("https://");
            ServerAction::bool_reply(ok)
        }
        c if c == tl_fns::bots::GetPopularAppBots::CONSTRUCTOR_ID => {
            let req = tl_fns::bots::GetPopularAppBots::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.limit, 100);
            if req.offset.is_empty() {
                ServerAction::ok(&tl_enums::bots::PopularAppBots::PopularAppBots(
                    tl_types::bots::PopularAppBots {
                        next_offset: Some("pop_2".to_owned()),
                        users: vec![
                            synthetic_bot_user(501, "app_one_bot", "App One", true),
                            synthetic_bot_user(502, "app_two_bot", "App Two", true),
                        ],
                    },
                ))
            } else {
                ServerAction::ok(&tl_enums::bots::PopularAppBots::PopularAppBots(
                    tl_types::bots::PopularAppBots {
                        next_offset: None,
                        users: vec![synthetic_bot_user(503, "app_three_bot", "App Three", true)],
                    },
                ))
            }
        }
        c if c == tl_fns::bots::GetRequestedWebViewButton::CONSTRUCTOR_ID => {
            let req = tl_fns::bots::GetRequestedWebViewButton::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.webapp_req_id, "req_chat_42");
            ServerAction::ok(&tl_enums::KeyboardButton::KeyboardButton(
                tl_types::KeyboardButton {
                    style: None,
                    text: "Pick Group".to_owned(),
                    r#type: tl_enums::ButtonType::RequestPeer(tl_types::ButtonTypeRequestPeer {
                        button_id: 77,
                        peer_type: tl_enums::RequestPeerType::Chat(tl_types::RequestPeerTypeChat {
                            creator: false,
                            bot_participant: true,
                            has_username: None,
                            forum: Some(false),
                            user_admin_rights: None,
                            bot_admin_rights: None,
                        }),
                        max_quantity: 1,
                    }),
                },
            ))
        }
        c if c == tl_fns::messages::SendBotRequestedPeer::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::SendBotRequestedPeer::from_bytes(&rpc.body).unwrap();
            // `bot_attach_web_view.cpp:2170-2175`: uses `f_webapp_req_id`, omits `f_msg_id`.
            assert!(req.msg_id.is_none());
            assert_eq!(req.webapp_req_id.as_deref(), Some("req_chat_42"));
            assert_eq!(req.button_id, 77);
            assert_eq!(req.requested_peers.len(), 1);
            ServerAction::ok(&synthetic_message_updates(
                302,
                tl_enums::Peer::User(tl_types::PeerUser { user_id: 500 }),
                "Shared peer",
            ))
        }
        c if c == tl_fns::bots::ToggleUserEmojiStatusPermission::CONSTRUCTOR_ID => {
            let req = tl_fns::bots::ToggleUserEmojiStatusPermission::from_bytes(&rpc.body).unwrap();
            ServerAction::bool_reply(req.enabled)
        }
        c if c == tl_fns::bots::GetBotRecommendations::CONSTRUCTOR_ID => {
            let _req = tl_fns::bots::GetBotRecommendations::from_bytes(&rpc.body).unwrap();
            ServerAction::ok(&tl_enums::users::Users::Slice(
                tl_types::users::UsersSlice {
                    count: 5,
                    users: vec![
                        synthetic_bot_user(601, "sim_one_bot", "Similar One", false),
                        synthetic_bot_user(602, "sim_two_bot", "Similar Two", true),
                    ],
                },
            ))
        }
        c if c == tl_fns::messages::GetPreparedInlineMessage::CONSTRUCTOR_ID => {
            let req = tl_fns::messages::GetPreparedInlineMessage::from_bytes(&rpc.body).unwrap();
            if req.id == "expired_id" {
                return ServerAction::err(400, "ID_EXPIRED");
            }
            ServerAction::ok(
                &tl_enums::messages::PreparedInlineMessage::PreparedInlineMessage(
                    tl_types::messages::PreparedInlineMessage {
                        query_id: 77_700,
                        result: tl_enums::BotInlineResult::BotInlineResult(
                            tl_types::BotInlineResult {
                                id: "prep_res_1".to_owned(),
                                r#type: "article".to_owned(),
                                title: Some("Invite Card".to_owned()),
                                description: Some("Prepared share".to_owned()),
                                url: None,
                                thumb: None,
                                content: None,
                                send_message: tl_enums::BotInlineMessage::Text(
                                    tl_types::BotInlineMessageText {
                                        no_webpage: false,
                                        invert_media: false,
                                        message: "Join fixture!".to_owned(),
                                        entities: None,
                                        reply_markup: None,
                                    },
                                ),
                            },
                        ),
                        peer_types: vec![
                            tl_enums::InlineQueryPeerType::PM,
                            tl_enums::InlineQueryPeerType::Chat,
                        ],
                        cache_time: 300,
                        users: vec![synthetic_bot_user(500, "fixture_bot", "Fixture Bot", true)],
                    },
                ),
            )
        }
        _ => ServerAction::err(400, "UNEXPECTED_RPC"),
    });

    let (rpc, _client) = make_mtproto_rpc(&server);
    let session = seed_session();
    let bot = input_user_for(&session, UserId(500));
    let bot_peer = input_peer_for(&session, PeerId::from_user(UserId(500)));
    let group_peer = input_peer_for(&session, PeerId::from_chat(ChatId(800)));

    // 1 & 2. bots.canSendMessage -> bots.allowSendMessage -> bots.canSendMessage
    assert!(
        !tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::bots::CanSendMessage { bot: bot.clone() })
        )
        .await
        .unwrap()
        .unwrap()
    );
    let _allow_upd = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::bots::AllowSendMessage { bot: bot.clone() }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::bots::CanSendMessage { bot: bot.clone() })
        )
        .await
        .unwrap()
        .unwrap()
    );

    // 3. bots.checkDownloadFileParams
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::bots::CheckDownloadFileParams {
                bot: bot.clone(),
                file_name: "report.pdf".to_owned(),
                url: "https://fixture.invalid/report.pdf".to_owned(),
            })
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert!(
        !tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::bots::CheckDownloadFileParams {
                bot: bot.clone(),
                file_name: "../evil.bin".to_owned(),
                url: "https://fixture.invalid/evil.bin".to_owned(),
            })
        )
        .await
        .unwrap()
        .unwrap()
    );

    // 4. bots.getPopularAppBots pagination (limit=100 matching kPopularAppBotsLimit)
    let tl_enums::bots::PopularAppBots::PopularAppBots(p1) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::bots::GetPopularAppBots {
            offset: String::new(),
            limit: 100,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(p1.users.len(), 2);
    assert_eq!(p1.next_offset.as_deref(), Some("pop_2"));

    let tl_enums::bots::PopularAppBots::PopularAppBots(p2) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::bots::GetPopularAppBots {
            offset: "pop_2".to_owned(),
            limit: 100,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(p2.users.len(), 1);
    assert_eq!(p2.next_offset, None);

    // 5. bots.getRequestedWebViewButton + messages.sendBotRequestedPeer(f_webapp_req_id)
    let tl_enums::KeyboardButton::KeyboardButton(btn) = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::bots::GetRequestedWebViewButton {
            bot: bot.clone(),
            webapp_req_id: "req_chat_42".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        btn.r#type,
        tl_enums::ButtonType::RequestPeer(ref r) if r.button_id == 77
    ));
    let _sent_peer = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::SendBotRequestedPeer {
            peer: bot_peer,
            msg_id: None,
            webapp_req_id: Some("req_chat_42".to_owned()),
            button_id: 77,
            requested_peers: vec![group_peer],
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // 6. bots.toggleUserEmojiStatusPermission
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::bots::ToggleUserEmojiStatusPermission {
                bot: bot.clone(),
                enabled: true,
            })
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert!(
        !tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::bots::ToggleUserEmojiStatusPermission {
                bot: bot.clone(),
                enabled: false,
            })
        )
        .await
        .unwrap()
        .unwrap()
    );

    // 7. bots.getBotRecommendations (UsersSlice count=5, users.len=2 -> more=3)
    let recs = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::bots::GetBotRecommendations { bot: bot.clone() }),
    )
    .await
    .unwrap()
    .unwrap();
    match recs {
        tl_enums::users::Users::Slice(s) => {
            assert_eq!(s.count, 5);
            assert_eq!(s.users.len(), 2);
        }
        other => panic!("expected UsersSlice, got {other:?}"),
    }

    // 8. messages.getPreparedInlineMessage (success + ID_EXPIRED error)
    let tl_enums::messages::PreparedInlineMessage::PreparedInlineMessage(prep) =
        tokio::time::timeout(
            TEST_TIMEOUT,
            rpc.invoke(&tl_fns::messages::GetPreparedInlineMessage {
                bot: bot.clone(),
                id: "prep_ok".to_owned(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prep.query_id, 77_700);
    assert_eq!(prep.peer_types.len(), 2);

    let err = tokio::time::timeout(
        TEST_TIMEOUT,
        rpc.invoke(&tl_fns::messages::GetPreparedInlineMessage {
            bot,
            id: "expired_id".to_owned(),
        }),
    )
    .await
    .unwrap()
    .expect_err("expected ID_EXPIRED");
    assert!(matches!(err, InvocationError::Rpc(ref e) if e.name == "ID_EXPIRED"));
}
