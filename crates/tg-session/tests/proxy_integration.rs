//! End-to-end local loopback proxy integration tests (zero real network).
//!
//! Spins up:
//! - an in-process fake MTProto 2.0 server (obfuscated2 + Intermediate/PaddedIntermediate +
//!   unencrypted `req_pq_multi` -> `resPQ` probes + encrypted `help.getNearestDc` RPCs),
//! - an in-process fake SOCKS5 proxy server (RFC 1928 + RFC 1929 username/password auth)
//!   relaying to the fake MTProto server,
//! - an in-process fake MTProxy (`dd` padded obfuscated2 secret) endpoint forwarding to the
//!   fake MTProto server,
//!
//! and verifies:
//! 1. Real RPCs route through SOCKS5 proxy #1, switching to SOCKS5 proxy #2 reconnects
//!    without losing the session / auth key, and subsequent RPCs go through proxy #2.
//! 2. Real RPCs route through a fake MTProxy (`dd` secret) endpoint.
//! 3. `tg_session::check_proxy` measures RTT against fake SOCKS5 and fake MTProxy and fails
//!    on unreachable endpoints.
//! 4. `ProxyRotationRunner` detects disconnect and rotates to a healthy backup proxy.
//! 5. `AccountManager` persists proxy settings into `tdata/settingss` and reloads them.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tg_api::{ApiCredentials, RpcClient as ApiRpcClient};
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType, SettingsProxy,
};
use tg_crypto::{AuthKey, CtrState, Side, decrypt_message_v2, sha256_bytes};
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, unpack_container, unwrap_gzip_layers,
};
use tg_mtproto::{Client as MtprotoClient, ClientConfig, DcOptions, Environment};
use tg_session::{
    AccountManager, ConnectionState, DEFAULT_PROXY_CHECK_TIMEOUT, MtprotoRpc, ProxyCheckFn,
    ProxyRotationRunner, WorkingDirOpenOptions, check_proxy, core_proxy_to_transport,
};
use tg_tl::mtproto::{
    CoreMessage, MsgContainer, enums as mtp_enums, functions as mtp_fns, types as mtp_types,
};
use tg_tl::{
    Deserializable, Identifiable, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_transport::{
    CONSTRUCTOR_REQ_PQ_MULTI, CONSTRUCTOR_RES_PQ, OBFUSCATED_HEADER_SIZE, PaddedIntermediate,
    ProxyData as TransportProxyData, ProxyType as TransportProxyType, TAG_INTERMEDIATE,
    TAG_PADDED_INTERMEDIATE,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Deterministic synthetic 256-byte MTProto auth key for local loopback tests.
fn test_auth_key(seed: u8) -> AuthKey {
    let mut raw = [0u8; 256];
    for (i, b) in raw.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(17));
    }
    AuthKey::from_bytes(&raw).expect("valid 256-byte key")
}

/// Derives AES-256 key from 32-byte nonce slice + optional 16-byte MTProxy secret.
fn derive_obf_key(raw: &[u8; 32], secret: Option<&[u8; 16]>) -> [u8; 32] {
    match secret {
        Some(sec) => {
            let mut input = [0u8; 48];
            input[..32].copy_from_slice(raw);
            input[32..].copy_from_slice(sec);
            sha256_bytes(&input)
        }
        None => *raw,
    }
}

/// Decodes a client's 64-byte Obfuscated2 header, returning `(recv_ctr, send_ctr, proto_tag, dc_id)`.
fn accept_obfuscated2(
    header: &[u8; OBFUSCATED_HEADER_SIZE],
    secret: Option<&[u8; 16]>,
) -> (CtrState, CtrState, [u8; 4], i16) {
    let mut raw_recv_key = [0u8; 32];
    raw_recv_key.copy_from_slice(&header[8..40]);
    let mut recv_iv = [0u8; 16];
    recv_iv.copy_from_slice(&header[40..56]);
    let recv_key = derive_obf_key(&raw_recv_key, secret);

    let mut reversed = [0u8; 48];
    reversed.copy_from_slice(&header[8..56]);
    reversed.reverse();
    let mut raw_send_key = [0u8; 32];
    raw_send_key.copy_from_slice(&reversed[0..32]);
    let mut send_iv = [0u8; 16];
    send_iv.copy_from_slice(&reversed[32..48]);
    let send_key = derive_obf_key(&raw_send_key, secret);

    let mut recv_ctr = CtrState::new(recv_key, recv_iv);
    let send_ctr = CtrState::new(send_key, send_iv);

    // Advance recv_ctr by 64 bytes and decrypt bytes 56..64 to recover protocol_tag & dc_id.
    let mut decrypted_hdr = *header;
    recv_ctr.decrypt(&mut decrypted_hdr);
    let proto_tag = [
        decrypted_hdr[56],
        decrypted_hdr[57],
        decrypted_hdr[58],
        decrypted_hdr[59],
    ];
    let dc_id = i16::from_le_bytes([decrypted_hdr[60], decrypted_hdr[61]]);
    (recv_ctr, send_ctr, proto_tag, dc_id)
}

async fn read_obf_frame(
    stream: &mut TcpStream,
    recv_ctr: &mut CtrState,
    padded: bool,
) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    recv_ctr.decrypt(&mut len_buf);
    let raw_len = u32::from_le_bytes(len_buf) & 0x7FFF_FFFF;
    let mut payload = vec![0u8; raw_len as usize];
    stream.read_exact(&mut payload).await?;
    recv_ctr.decrypt(&mut payload);
    if padded {
        Ok(PaddedIntermediate::strip_mtproto_padding(&payload).to_vec())
    } else {
        Ok(payload)
    }
}

async fn write_obf_frame(
    stream: &mut TcpStream,
    send_ctr: &mut CtrState,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut wire = Vec::with_capacity(4 + payload.len());
    wire.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    wire.extend_from_slice(payload);
    send_ctr.encrypt(&mut wire);
    stream.write_all(&wire).await?;
    stream.flush().await
}

/// Unwraps `invokeWithLayer(initConnection(inner))` / `invokeAfterMsg(inner)` to inspect inner call.
fn unwrap_tl_call(body: &[u8]) -> &[u8] {
    let mut cur = body;
    loop {
        if cur.len() < 4 {
            return cur;
        }
        let ctor = u32::from_le_bytes([cur[0], cur[1], cur[2], cur[3]]);
        if ctor == tl_fns::InvokeWithLayer::<tg_tl::RawObject>::CONSTRUCTOR_ID {
            // ctor(4) + layer(4) + query
            if cur.len() > 8 {
                cur = &cur[8..];
                continue;
            }
        }
        if ctor == tl_fns::InitConnection::<tg_tl::RawObject>::CONSTRUCTOR_ID {
            // Skip past initConnection fields via Deserializable on RawObject:
            if let Ok(init) = tl_fns::InitConnection::<tg_tl::RawObject>::from_bytes(cur) {
                // Find the serialized query bytes at the tail of `cur`.
                let qlen = init.query.0.len();
                if qlen <= cur.len() {
                    cur = &cur[cur.len() - qlen..];
                    continue;
                }
            }
        }
        if ctor == tl_fns::InvokeAfterMsg::<tg_tl::RawObject>::CONSTRUCTOR_ID && cur.len() > 12 {
            cur = &cur[12..];
            continue;
        }
        return cur;
    }
}

/// Builds a synthetic unencrypted `resPQ#05162463` reply echoing `nonce`.
fn build_fake_res_pq(client_msg_id: u64, nonce: [u8; 16]) -> Vec<u8> {
    let mut body = Vec::with_capacity(64);
    body.extend_from_slice(&CONSTRUCTOR_RES_PQ.to_le_bytes());
    body.extend_from_slice(&nonce);
    body.extend_from_slice(&[0xAB; 16]); // server_nonce
    // pq bytes (8 bytes encoded as TL string: len=8, 8 bytes, 3 pad bytes = 12 bytes)
    body.push(8);
    body.extend_from_slice(&[0x17, 0xED, 0x48, 0x94, 0x1A, 0x08, 0xF9, 0x81]);
    body.extend_from_slice(&[0, 0, 0]);
    // server_public_key_fingerprints: vector#1cb5c415 len=0
    body.extend_from_slice(&0x1CB5_C415u32.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    let server_msg_id = (client_msg_id & !3) | 1;
    encode_unencrypted_packet(server_msg_id, &body)
}

/// Handles one MTProto connection (either direct/SOCKS5 with `secret = None`, or MTProxy `dd`
/// with `secret = Some(16-byte key)`). Responds to `req_pq_multi` probes and encrypted
/// `help.getNearestDc` RPCs with the configured `country` code.
async fn serve_mtproto_connection(
    mut stream: TcpStream,
    auth_key: AuthKey,
    secret: Option<[u8; 16]>,
    country: String,
    rpc_counter: Arc<AtomicUsize>,
    probe_counter: Arc<AtomicUsize>,
) {
    let mut hdr = [0u8; OBFUSCATED_HEADER_SIZE];
    if stream.read_exact(&mut hdr).await.is_err() {
        return;
    }
    let (mut recv_ctr, mut send_ctr, proto_tag, _dc_id) = accept_obfuscated2(&hdr, secret.as_ref());
    let padded = match proto_tag {
        TAG_INTERMEDIATE => false,
        TAG_PADDED_INTERMEDIATE => true,
        _ => return,
    };

    let mut server_seq: u32 = 1;
    let mut msg_id_seq: u64 = 0;

    while let Ok(packet) = read_obf_frame(&mut stream, &mut recv_ctr, padded).await {
        if packet.len() < 24 {
            continue;
        }
        let key_id = u64::from_le_bytes(packet[0..8].try_into().unwrap());
        if key_id == 0 {
            // Unencrypted client probe (req_pq_multi): client msg_id has low 2 bits == 0.
            let client_msg_id = u64::from_le_bytes(packet[8..16].try_into().unwrap());
            let body_len = u32::from_le_bytes(packet[16..20].try_into().unwrap()) as usize;
            if body_len >= 20 && 20 + body_len <= packet.len() {
                let body = &packet[20..20 + body_len];
                if u32::from_le_bytes(body[0..4].try_into().unwrap()) == CONSTRUCTOR_REQ_PQ_MULTI {
                    probe_counter.fetch_add(1, Ordering::Relaxed);
                    let mut nonce = [0u8; 16];
                    nonce.copy_from_slice(&body[4..20]);
                    let reply = build_fake_res_pq(client_msg_id, nonce);
                    if write_obf_frame(&mut stream, &mut send_ctr, &reply)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
            continue;
        }

        // Encrypted MTProto 2.0 packet.
        let Ok(dec) = decrypt_message_v2(&packet, &auth_key, Side::Client) else {
            continue;
        };
        let mut inner_messages: Vec<(u64, Vec<u8>)> = Vec::new();
        if dec.body.len() >= 4
            && u32::from_le_bytes(dec.body[0..4].try_into().unwrap())
                == MsgContainer::CONSTRUCTOR_ID
        {
            if let Ok(messages) = unpack_container(&dec.body) {
                for CoreMessage { msg_id, body, .. } in messages {
                    inner_messages.push((msg_id as u64, body.0));
                }
            }
        } else {
            inner_messages.push((dec.msg_id as u64, dec.body));
        }

        for (req_msg_id, raw_body) in inner_messages {
            let mut budget = 1024 * 1024;
            let body = unwrap_gzip_layers(&raw_body, &mut budget, 0).unwrap_or(raw_body);
            let unwrapped = unwrap_tl_call(&body);
            if unwrapped.len() < 4 {
                continue;
            }
            let ctor = u32::from_le_bytes(unwrapped[0..4].try_into().unwrap());
            if ctor == tl_fns::help::GetNearestDc::CONSTRUCTOR_ID {
                rpc_counter.fetch_add(1, Ordering::Relaxed);
                let nearest = tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: country.clone(),
                    this_dc: 2,
                    nearest_dc: 2,
                });
                // Wire-format `rpc_result#f35c6d01 req_msg_id:long result:Object`.
                let mut result_bytes = Vec::with_capacity(32);
                result_bytes.extend_from_slice(&0xF35C_6D01u32.to_le_bytes());
                result_bytes.extend_from_slice(&(req_msg_id as i64).to_le_bytes());
                nearest.serialize(&mut result_bytes);
                msg_id_seq += 1;
                let server_msg_id = ((req_msg_id & !3).wrapping_add(msg_id_seq * 4)) | 1;
                let reply = build_encrypted_packet(
                    dec.salt as u64,
                    dec.session_id as u64,
                    server_msg_id,
                    server_seq,
                    &result_bytes,
                    &auth_key,
                    Side::Server,
                );
                server_seq += 2;
                if write_obf_frame(&mut stream, &mut send_ctr, &reply)
                    .await
                    .is_err()
                {
                    return;
                }
            } else if ctor == mtp_fns::Ping::CONSTRUCTOR_ID
                && let Ok(ping) = mtp_fns::Ping::from_bytes(unwrapped)
            {
                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                    msg_id: req_msg_id as i64,
                    ping_id: ping.ping_id,
                });
                msg_id_seq += 1;
                let server_msg_id = ((req_msg_id & !3).wrapping_add(msg_id_seq * 4)) | 1;
                let reply = build_encrypted_packet(
                    dec.salt as u64,
                    dec.session_id as u64,
                    server_msg_id,
                    server_seq,
                    &pong.to_bytes(),
                    &auth_key,
                    Side::Server,
                );
                server_seq += 2;
                let _ = write_obf_frame(&mut stream, &mut send_ctr, &reply).await;
            } else if ctor == mtp_fns::PingDelayDisconnect::CONSTRUCTOR_ID
                && let Ok(ping) = mtp_fns::PingDelayDisconnect::from_bytes(unwrapped)
            {
                let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                    msg_id: req_msg_id as i64,
                    ping_id: ping.ping_id,
                });
                msg_id_seq += 1;
                let server_msg_id = ((req_msg_id & !3).wrapping_add(msg_id_seq * 4)) | 1;
                let reply = build_encrypted_packet(
                    dec.salt as u64,
                    dec.session_id as u64,
                    server_msg_id,
                    server_seq,
                    &pong.to_bytes(),
                    &auth_key,
                    Side::Server,
                );
                server_seq += 2;
                let _ = write_obf_frame(&mut stream, &mut send_ctr, &reply).await;
            }
        }
    }
}

struct FakeMtprotoServer {
    addr: SocketAddr,
    rpc_count: Arc<AtomicUsize>,
    probe_count: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl FakeMtprotoServer {
    async fn spawn(auth_key: AuthKey, secret: Option<[u8; 16]>, country: &str) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let rpc_count = Arc::new(AtomicUsize::new(0));
        let probe_count = Arc::new(AtomicUsize::new(0));
        let rpc_bg = Arc::clone(&rpc_count);
        let probe_bg = Arc::clone(&probe_count);
        let country_owned = country.to_owned();

        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let key = auth_key.clone();
                let c = country_owned.clone();
                let rc = Arc::clone(&rpc_bg);
                let pc = Arc::clone(&probe_bg);
                tokio::spawn(async move {
                    serve_mtproto_connection(stream, key, secret, c, rc, pc).await;
                });
            }
        });

        Self {
            addr,
            rpc_count,
            probe_count,
            task,
        }
    }
}

impl Drop for FakeMtprotoServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Local in-process fake SOCKS5 proxy server (RFC 1928 + RFC 1929 user/pass auth).
struct FakeSocks5Proxy {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl FakeSocks5Proxy {
    async fn spawn(expected_auth: Option<(String, String)>) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let conns_bg = Arc::clone(&connections);

        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let auth = expected_auth.clone();
                let counter = Arc::clone(&conns_bg);
                tokio::spawn(async move {
                    // 1. Greeting: VER(5), NMETHODS, METHODS...
                    let mut hdr = [0u8; 2];
                    if client.read_exact(&mut hdr).await.is_err() || hdr[0] != 0x05 {
                        return;
                    }
                    let mut methods = vec![0u8; hdr[1] as usize];
                    if client.read_exact(&mut methods).await.is_err() {
                        return;
                    }
                    if let Some((exp_user, exp_pass)) = auth {
                        if !methods.contains(&0x02) {
                            let _ = client.write_all(&[0x05, 0xFF]).await;
                            return;
                        }
                        if client.write_all(&[0x05, 0x02]).await.is_err() {
                            return;
                        }
                        // RFC 1929 subnegotiation: VER(1), ULEN, UNAME, PLEN, PASSWD
                        let mut ver_ulen = [0u8; 2];
                        if client.read_exact(&mut ver_ulen).await.is_err() || ver_ulen[0] != 0x01 {
                            return;
                        }
                        let mut user = vec![0u8; ver_ulen[1] as usize];
                        if client.read_exact(&mut user).await.is_err() {
                            return;
                        }
                        let mut plen = [0u8; 1];
                        if client.read_exact(&mut plen).await.is_err() {
                            return;
                        }
                        let mut pass = vec![0u8; plen[0] as usize];
                        if client.read_exact(&mut pass).await.is_err() {
                            return;
                        }
                        let ok = user == exp_user.as_bytes() && pass == exp_pass.as_bytes();
                        if client
                            .write_all(&[0x01, if ok { 0x00 } else { 0x01 }])
                            .await
                            .is_err()
                            || !ok
                        {
                            return;
                        }
                    } else if client.write_all(&[0x05, 0x00]).await.is_err() {
                        return;
                    }

                    // 2. CONNECT request: VER(5), CMD(1), RSV(0), ATYP, DST.ADDR, DST.PORT
                    let mut req = [0u8; 4];
                    if client.read_exact(&mut req).await.is_err()
                        || req[0] != 0x05
                        || req[1] != 0x01
                    {
                        return;
                    }
                    let target: String = match req[3] {
                        0x01 => {
                            let mut ip = [0u8; 4];
                            if client.read_exact(&mut ip).await.is_err() {
                                return;
                            }
                            let mut port = [0u8; 2];
                            if client.read_exact(&mut port).await.is_err() {
                                return;
                            }
                            format!("{}:{}", Ipv4Addr::from(ip), u16::from_be_bytes(port))
                        }
                        0x03 => {
                            let mut dlen = [0u8; 1];
                            if client.read_exact(&mut dlen).await.is_err() {
                                return;
                            }
                            let mut dom = vec![0u8; dlen[0] as usize];
                            if client.read_exact(&mut dom).await.is_err() {
                                return;
                            }
                            let mut port = [0u8; 2];
                            if client.read_exact(&mut port).await.is_err() {
                                return;
                            }
                            format!(
                                "{}:{}",
                                String::from_utf8_lossy(&dom),
                                u16::from_be_bytes(port)
                            )
                        }
                        _ => return,
                    };

                    let Ok(mut upstream) = TcpStream::connect(&target).await else {
                        let _ = client
                            .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                            .await;
                        return;
                    };
                    counter.fetch_add(1, Ordering::Relaxed);
                    // Reply: succeeded, bound 127.0.0.1:0.
                    if client
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });

        Self {
            addr,
            connections,
            task,
        }
    }
}

impl Drop for FakeSocks5Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Local in-process fake MTProxy (`dd` secret) relay sitting in front of a `FakeMtprotoServer`.
///
/// Terminates the client's secret-mixed Obfuscated2 + PaddedIntermediate transport, strips
/// padding, and relays every MTProto packet to an upstream plain Obfuscated2 + Intermediate
/// `FakeMtprotoServer` (just like a real MTProxy middle-box).
struct FakeMtProxyRelay {
    addr: SocketAddr,
    relayed_packets: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl FakeMtProxyRelay {
    async fn spawn(secret: [u8; 16], upstream_addr: SocketAddr) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let relayed_packets = Arc::new(AtomicUsize::new(0));
        let counter_bg = Arc::clone(&relayed_packets);

        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let counter = Arc::clone(&counter_bg);
                tokio::spawn(async move {
                    let mut hdr = [0u8; OBFUSCATED_HEADER_SIZE];
                    if client.read_exact(&mut hdr).await.is_err() {
                        return;
                    }
                    let (mut c_recv, mut c_send, proto_tag, dc_id) =
                        accept_obfuscated2(&hdr, Some(&secret));
                    if proto_tag != TAG_PADDED_INTERMEDIATE {
                        return;
                    }

                    // Dial upstream FakeMtprotoServer with plain (unsecret) Obfuscated2 + Intermediate.
                    let Ok(mut upstream) = TcpStream::connect(upstream_addr).await else {
                        return;
                    };
                    let (mut up_obf, up_hdr) =
                        tg_transport::Obfuscated2::generate(TAG_INTERMEDIATE, dc_id, None);
                    if upstream.write_all(&up_hdr).await.is_err() {
                        return;
                    }

                    let (mut c_r, mut c_w) = client.into_split();
                    let (mut u_r, mut u_w) = upstream.into_split();

                    // Reconstruct CTR states for upstream: Obfuscated2 uses encrypt/decrypt independently.
                    // We spawn a single bidirectional packet pump owning `up_obf` via tokio::select!
                    let mut c_len_buf = [0u8; 4];
                    let mut u_len_buf = [0u8; 4];
                    loop {
                        tokio::select! {
                            res = c_r.read_exact(&mut c_len_buf) => {
                                if res.is_err() { break; }
                                c_recv.decrypt(&mut c_len_buf);
                                let raw_len = (u32::from_le_bytes(c_len_buf) & 0x7FFF_FFFF) as usize;
                                let mut payload = vec![0u8; raw_len];
                                if c_r.read_exact(&mut payload).await.is_err() { break; }
                                c_recv.decrypt(&mut payload);
                                let clean = PaddedIntermediate::strip_mtproto_padding(&payload);
                                let mut wire = Vec::with_capacity(4 + clean.len());
                                wire.extend_from_slice(&(clean.len() as u32).to_le_bytes());
                                wire.extend_from_slice(clean);
                                up_obf.encrypt_outgoing(&mut wire);
                                if u_w.write_all(&wire).await.is_err() { break; }
                                let _ = u_w.flush().await;
                                counter.fetch_add(1, Ordering::Relaxed);
                            }
                            res = u_r.read_exact(&mut u_len_buf) => {
                                if res.is_err() { break; }
                                up_obf.decrypt_incoming(&mut u_len_buf);
                                let raw_len = (u32::from_le_bytes(u_len_buf) & 0x7FFF_FFFF) as usize;
                                let mut payload = vec![0u8; raw_len];
                                if u_r.read_exact(&mut payload).await.is_err() { break; }
                                up_obf.decrypt_incoming(&mut payload);
                                let mut wire = Vec::with_capacity(4 + payload.len());
                                wire.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                                wire.extend_from_slice(&payload);
                                c_send.encrypt(&mut wire);
                                if c_w.write_all(&wire).await.is_err() { break; }
                                let _ = c_w.flush().await;
                            }
                        }
                    }
                });
            }
        });

        Self {
            addr,
            relayed_packets,
            task,
        }
    }
}

impl Drop for FakeMtProxyRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Local in-process fake HTTP CONNECT proxy (with optional Basic auth).
struct FakeHttpConnectProxy {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl FakeHttpConnectProxy {
    async fn spawn(expected_basic_header: Option<String>) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let conns_bg = Arc::clone(&connections);

        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let auth = expected_basic_header.clone();
                let counter = Arc::clone(&conns_bg);
                tokio::spawn(async move {
                    let mut buf = Vec::with_capacity(512);
                    let mut byte = [0u8; 1];
                    while buf.len() < 4096 {
                        if client.read_exact(&mut byte).await.is_err() {
                            return;
                        }
                        buf.push(byte[0]);
                        if buf.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let req = String::from_utf8_lossy(&buf);
                    let mut lines = req.lines();
                    let Some(first_line) = lines.next() else {
                        return;
                    };
                    let parts: Vec<&str> = first_line.split_whitespace().collect();
                    if parts.len() < 2 || parts[0] != "CONNECT" {
                        let _ = client
                            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")
                            .await;
                        return;
                    }
                    if let Some(expected) = auth {
                        let expected_lower = expected.to_ascii_lowercase();
                        let has_auth = lines.any(|l| {
                            l.to_ascii_lowercase().starts_with("proxy-authorization:")
                                && l.to_ascii_lowercase().contains(&expected_lower)
                        });
                        if !has_auth {
                            let _ = client
                                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                                .await;
                            return;
                        }
                    }
                    let target = parts[1];
                    let Ok(mut upstream) = TcpStream::connect(target).await else {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                        return;
                    };
                    counter.fetch_add(1, Ordering::Relaxed);
                    if client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });

        Self {
            addr,
            connections,
            task,
        }
    }
}

impl Drop for FakeHttpConnectProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn configure_local_dcs(dc_options: &DcOptions, entries: &[(i32, u16)]) {
    let opts: Vec<tl_enums::DcOption> = entries
        .iter()
        .map(|&(dc_id, port)| {
            tl_enums::DcOption::DcOption(tl_types::DcOption {
                ipv6: false,
                media_only: false,
                tcpo_only: true,
                cdn: false,
                r#static: true,
                this_port_only: true,
                id: dc_id,
                ip_address: "127.0.0.1".to_owned(),
                port: i32::from(port),
                secret: None,
            })
        })
        .collect();
    dc_options.set_from_list(&opts);
}

fn configure_local_dc(dc_options: &DcOptions, dc_id: i32, port: u16) {
    configure_local_dcs(dc_options, &[(dc_id, port)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_through_socks5_proxy_and_live_switch_preserves_all_dc_sessions() {
    let key_dc2 = test_auth_key(0x2A);
    let key_dc4 = test_auth_key(0x4B);
    let mtp_dc2 = FakeMtprotoServer::spawn(key_dc2.clone(), None, "US").await;
    let mtp_dc4 = FakeMtprotoServer::spawn(key_dc4.clone(), None, "NL").await;
    let socks1 = FakeSocks5Proxy::spawn(Some(("alice".to_owned(), "s3cr3t".to_owned()))).await;
    let socks2 = FakeSocks5Proxy::spawn(None).await;

    let proxy1 = TransportProxyData {
        proxy_type: TransportProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: socks1.addr.port(),
        user: "alice".to_owned(),
        password: "s3cr3t".to_owned(),
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    };
    let proxy2 = TransportProxyData {
        proxy_type: TransportProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: socks2.addr.port(),
        user: String::new(),
        password: String::new(),
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    };

    let cfg = ClientConfig {
        main_dc_id: 2,
        proxy: proxy1.clone(),
        use_ipv6: false,
        use_pfs: false,
        ..ClientConfig::default()
    };

    let (client, _updates_rx) = MtprotoClient::new(cfg);
    configure_local_dcs(
        client.dc_options(),
        &[(2, mtp_dc2.addr.port()), (4, mtp_dc4.addr.port())],
    );
    client.import_persistent_keys(vec![(2, key_dc2.clone()), (4, key_dc4.clone())]);
    let rpc = MtprotoRpc::new(client.clone());

    // 1. Invoke RPCs on BOTH DC 2 and DC 4 through SOCKS5 proxy #1 (with username/password auth).
    let res1_dc2 = tokio::time::timeout(
        Duration::from_secs(5),
        ApiRpcClient::invoke(&rpc, &tl_fns::help::GetNearestDc),
    )
    .await
    .expect("DC2 RPC #1 timed out")
    .expect("DC2 RPC #1 through SOCKS5 proxy #1 should succeed");
    let tl_enums::NearestDc::NearestDc(ndc1_dc2) = res1_dc2;
    assert_eq!(ndc1_dc2.country, "US");

    let res1_dc4 = tokio::time::timeout(
        Duration::from_secs(5),
        ApiRpcClient::invoke_in_dc(&rpc, 4, &tl_fns::help::GetNearestDc),
    )
    .await
    .expect("DC4 RPC #1 timed out")
    .expect("DC4 RPC #1 through SOCKS5 proxy #1 should succeed");
    let tl_enums::NearestDc::NearestDc(ndc1_dc4) = res1_dc4;
    assert_eq!(ndc1_dc4.country, "NL");

    assert!(socks1.connections.load(Ordering::Relaxed) >= 2);
    assert_eq!(socks2.connections.load(Ordering::Relaxed), 0);
    assert_eq!(client.connection_state(), ConnectionState::Connected);

    // 2. Live-switch to SOCKS5 proxy #2 without dropping any DC session or auth key.
    rpc.set_proxy(proxy2.clone());
    assert_eq!(rpc.proxy(), proxy2);

    let res2_dc2 = tokio::time::timeout(
        Duration::from_secs(5),
        ApiRpcClient::invoke(&rpc, &tl_fns::help::GetNearestDc),
    )
    .await
    .expect("DC2 RPC #2 timed out")
    .expect("DC2 RPC #2 through SOCKS5 proxy #2 should succeed");
    let tl_enums::NearestDc::NearestDc(ndc2_dc2) = res2_dc2;
    assert_eq!(ndc2_dc2.country, "US");

    let res2_dc4 = tokio::time::timeout(
        Duration::from_secs(5),
        ApiRpcClient::invoke_in_dc(&rpc, 4, &tl_fns::help::GetNearestDc),
    )
    .await
    .expect("DC4 RPC #2 timed out")
    .expect("DC4 RPC #2 through SOCKS5 proxy #2 should succeed");
    let tl_enums::NearestDc::NearestDc(ndc2_dc4) = res2_dc4;
    assert_eq!(ndc2_dc4.country, "NL");

    assert!(
        socks2.connections.load(Ordering::Relaxed) >= 2,
        "both DC 2 and DC 4 must have reconnected through SOCKS5 proxy #2"
    );

    // Both DC persistent auth keys were preserved across the proxy switch!
    let exported = client.export_persistent_keys();
    assert_eq!(exported.len(), 2);
    assert!(
        exported
            .iter()
            .any(|(dc, k)| *dc == 2 && k.key_id() == key_dc2.key_id())
    );
    assert!(
        exported
            .iter()
            .any(|(dc, k)| *dc == 4 && k.key_id() == key_dc4.key_id())
    );
    assert!(mtp_dc2.rpc_count.load(Ordering::Relaxed) >= 2);
    assert!(mtp_dc4.rpc_count.load(Ordering::Relaxed) >= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_through_mtproxy_dd_relay_in_front_of_mtproto_server_succeeds() {
    let key = test_auth_key(0x7C);
    // 17-byte `dd` MTProxy secret: 0xDD prefix + 16-byte AES-CTR secret key.
    let secret_key = [
        0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98, 0xA9, 0xBA, 0xCB, 0xDC, 0xED, 0xFE,
        0x0F,
    ];
    let mut dd_hex = String::from("dd");
    for b in secret_key {
        dd_hex.push_str(&format!("{b:02x}"));
    }

    // Separate FakeMtprotoServer behind a true FakeMtProxyRelay endpoint.
    let mtp_server = FakeMtprotoServer::spawn(key.clone(), None, "DE").await;
    let mtproxy_relay = FakeMtProxyRelay::spawn(secret_key, mtp_server.addr).await;

    let mtproxy = TransportProxyData {
        proxy_type: TransportProxyType::Mtproto,
        host: "127.0.0.1".to_owned(),
        port: mtproxy_relay.addr.port(),
        user: String::new(),
        password: dd_hex,
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    };
    assert!(mtproxy.valid());

    let cfg = ClientConfig {
        main_dc_id: 2,
        proxy: mtproxy.clone(),
        use_ipv6: false,
        use_pfs: false,
        ..ClientConfig::default()
    };

    let (client, _updates_rx) = MtprotoClient::new(cfg);
    client.import_persistent_keys(vec![(2, key)]);

    let res = tokio::time::timeout(
        Duration::from_secs(5),
        client.invoke(&tl_fns::help::GetNearestDc),
    )
    .await
    .expect("MTProxy RPC timed out")
    .expect("MTProxy RPC should succeed");
    let tl_enums::NearestDc::NearestDc(ndc) = res;
    assert_eq!(ndc.country, "DE");
    assert!(mtproxy_relay.relayed_packets.load(Ordering::Relaxed) >= 1);
    assert!(mtp_server.rpc_count.load(Ordering::Relaxed) >= 1);

    // Also check real `check_proxy` probe through the MTProxy relay to the upstream server.
    let dc_options = DcOptions::new(Environment::Production);
    let rtt = check_proxy(2, &dc_options, &mtproxy, false, Duration::from_secs(3))
        .await
        .expect("check_proxy through MTProxy relay should succeed");
    assert!(rtt < Duration::from_secs(3));
    assert!(mtp_server.probe_count.load(Ordering::Relaxed) >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_and_check_through_http_connect_proxy_and_abort_stalled_proxy() {
    let key = test_auth_key(0x5E);
    let mtp_server = FakeMtprotoServer::spawn(key.clone(), None, "FR").await;
    // Basic auth header for `bob:hunter2` -> base64("bob:hunter2") = "Ym9iOmh1bnRlcjI="
    let http_proxy_srv =
        FakeHttpConnectProxy::spawn(Some("Basic Ym9iOmh1bnRlcjI=".to_owned())).await;

    // Also bind a blackhole proxy that accepts TCP connections and never replies,
    // proving `set_proxy` aborts an in-flight stalled dial immediately.
    let blackhole_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let blackhole_port = blackhole_listener.local_addr().unwrap().port();
    let blackhole_task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = blackhole_listener.accept().await {
            held.push(stream);
        }
    });

    let stalled_proxy = TransportProxyData::socks5("127.0.0.1", blackhole_port, "", "");
    let http_proxy =
        TransportProxyData::http("127.0.0.1", http_proxy_srv.addr.port(), "bob", "hunter2");

    let cfg = ClientConfig {
        main_dc_id: 2,
        proxy: stalled_proxy,
        use_ipv6: false,
        use_pfs: false,
        ..ClientConfig::default()
    };

    let (client, _updates_rx) = MtprotoClient::new(cfg);
    configure_local_dc(client.dc_options(), 2, mtp_server.addr.port());
    client.import_persistent_keys(vec![(2, key)]);
    let rpc = MtprotoRpc::new(client.clone());

    // Start an RPC while configured with the blackhole proxy (it will stall in SOCKS5 handshake).
    let rpc_bg = rpc.clone();
    let rpc_fut =
        tokio::spawn(
            async move { ApiRpcClient::invoke(&rpc_bg, &tl_fns::help::GetNearestDc).await },
        );
    tokio::time::sleep(Duration::from_millis(120)).await;

    // Switch to the working HTTP CONNECT proxy: must abort the stalled connect immediately!
    rpc.set_proxy(http_proxy.clone());
    let res = tokio::time::timeout(Duration::from_secs(3), rpc_fut)
        .await
        .expect("switching away from stalled proxy must not wait for 8s connect timeout")
        .expect("task join")
        .expect("RPC over HTTP CONNECT proxy should succeed");
    let tl_enums::NearestDc::NearestDc(ndc) = res;
    assert_eq!(ndc.country, "FR");
    assert!(http_proxy_srv.connections.load(Ordering::Relaxed) >= 1);

    // Real check_proxy over HTTP CONNECT proxy also succeeds.
    let rtt = check_proxy(
        2,
        client.dc_options(),
        &http_proxy,
        false,
        Duration::from_secs(3),
    )
    .await
    .expect("HTTP CONNECT check_proxy should succeed");
    assert!(rtt < Duration::from_secs(3));

    blackhole_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_proxy_measures_ping_for_socks5_and_mtproxy_and_fails_on_closed_port() {
    let key = test_auth_key(0x33);
    let mtp_server = FakeMtprotoServer::spawn(key.clone(), None, "NL").await;
    let socks = FakeSocks5Proxy::spawn(None).await;

    let dc_options = DcOptions::new(Environment::Production);
    configure_local_dc(&dc_options, 2, mtp_server.addr.port());

    // 1. SOCKS5 real check_proxy succeeds and triggers req_pq_multi probe.
    let socks_proxy = TransportProxyData {
        proxy_type: TransportProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: socks.addr.port(),
        user: String::new(),
        password: String::new(),
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    };
    let rtt = check_proxy(2, &dc_options, &socks_proxy, false, Duration::from_secs(3))
        .await
        .expect("SOCKS5 check_proxy should succeed");
    assert!(rtt < Duration::from_secs(3));
    assert!(mtp_server.probe_count.load(Ordering::Relaxed) >= 1);

    // 2. MTProxy real check_proxy succeeds.
    let secret_key = [0x42u8; 16];
    let mtproxy_ep = FakeMtprotoServer::spawn(key, Some(secret_key), "NL").await;
    let mtproxy = TransportProxyData {
        proxy_type: TransportProxyType::Mtproto,
        host: "127.0.0.1".to_owned(),
        port: mtproxy_ep.addr.port(),
        user: String::new(),
        password: format!("dd{}", "42".repeat(16)),
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    };
    let rtt_mtp = check_proxy(2, &dc_options, &mtproxy, false, Duration::from_secs(3))
        .await
        .expect("MTProxy check_proxy should succeed");
    assert!(rtt_mtp < Duration::from_secs(3));
    assert!(mtproxy_ep.probe_count.load(Ordering::Relaxed) >= 1);

    // 3. Unbound/closed local port fails cleanly.
    let dead_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let closed_port = dead_listener.local_addr().unwrap().port();
    drop(dead_listener);
    let dead_proxy = TransportProxyData {
        proxy_type: TransportProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: closed_port,
        user: String::new(),
        password: String::new(),
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    };
    let err = check_proxy(2, &dc_options, &dead_proxy, false, Duration::from_secs(2)).await;
    assert!(err.is_err(), "closed proxy port must fail check_proxy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_rotation_runner_automatically_switches_from_dead_proxy_to_healthy_backup() {
    let bad_proxy = CoreProxyData {
        proxy_type: CoreProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: 19991,
        user: String::new(),
        password: String::new(),
    };
    let good_proxy = CoreProxyData {
        proxy_type: CoreProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: 19992,
        user: String::new(),
        password: String::new(),
    };

    let mut settings = SettingsProxy::new();
    settings.set_list(vec![bad_proxy.clone(), good_proxy.clone()]);
    settings.set_selected(bad_proxy.clone());
    settings.set_settings(ProxySettingsMode::Enabled);
    settings.set_proxy_rotation_enabled(true);
    settings.set_proxy_rotation_timeout(1); // 1 second timeout for fast test

    let cfg = ClientConfig {
        proxy: core_proxy_to_transport(&bad_proxy),
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = MtprotoClient::new(cfg);

    let rotated_to: Arc<Mutex<Option<CoreProxyData>>> = Arc::new(Mutex::new(None));
    let rotated_cb_slot = Arc::clone(&rotated_to);
    let good_for_check = good_proxy.clone();

    let checker: ProxyCheckFn = Arc::new(move |proxy: CoreProxyData, _try_ipv6: bool| {
        let is_good = proxy == good_for_check;
        Box::pin(async move {
            if is_good {
                Ok(Duration::from_millis(12))
            } else {
                Err("connection refused".to_owned())
            }
        })
    });

    let shared_settings = Arc::new(Mutex::new(settings));
    let runner = ProxyRotationRunner::spawn_with_checker(
        &tokio::runtime::Handle::current(),
        client.clone(),
        shared_settings,
        checker,
        Duration::from_millis(50),
        Some(Arc::new(move |_updated_settings, target| {
            *rotated_cb_slot.lock() = Some(target.clone());
        })),
    );

    // Wait up to 4 seconds for rotation to fire after the 1s timeout.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        if rotated_to.lock().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(rotated_to.lock().clone(), Some(good_proxy.clone()));
    assert_eq!(client.proxy(), core_proxy_to_transport(&good_proxy));
    assert_eq!(runner.settings().selected(), &good_proxy);
    drop(runner);
}

#[test]
fn account_manager_persists_and_reloads_proxy_settings_in_tdata() {
    let dir = tempfile::tempdir().unwrap();
    let creds = ApiCredentials {
        api_id: 12345,
        api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
    };
    let opts = WorkingDirOpenOptions::new(dir.path(), creds);

    let mut mgr = AccountManager::open(opts.clone()).expect("open fresh tdata");
    let mut proxy_settings = SettingsProxy::new();
    let p = CoreProxyData {
        proxy_type: CoreProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: 1088,
        user: "u".to_owned(),
        password: "p".to_owned(),
    };
    proxy_settings.add_to_list(p.clone());
    proxy_settings.set_selected(p.clone());
    proxy_settings.set_settings(ProxySettingsMode::Enabled);
    proxy_settings.set_use_proxy_for_calls(true);
    proxy_settings.set_try_ipv6(true);
    proxy_settings.set_proxy_rotation_enabled(true);
    proxy_settings.set_proxy_rotation_timeout(15);

    mgr.set_proxy_settings(proxy_settings)
        .expect("save proxy settings");
    drop(mgr);

    let reopened = AccountManager::open(opts).expect("reopen tdata");
    let loaded = reopened.proxy_settings();
    assert_eq!(loaded.settings(), ProxySettingsMode::Enabled);
    assert_eq!(loaded.selected(), &p);
    assert_eq!(loaded.list(), &[p]);
    assert!(loaded.use_proxy_for_calls());
    assert!(loaded.try_ipv6());
    assert!(loaded.proxy_rotation_enabled());
    assert_eq!(loaded.proxy_rotation_timeout(), 15);
    assert_eq!(DEFAULT_PROXY_CHECK_TIMEOUT, Duration::from_secs(8));
}
