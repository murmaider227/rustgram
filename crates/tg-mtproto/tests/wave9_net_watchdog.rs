//! Wave 9-U (TGB-3147): a connection that goes silent is restarted, like tdesktop's receive
//! watchdog (`onSentSome` / `waitReceivedFailed`, 4-64 s) and ping timer (`sendPingByTimer`,
//! 45 s). All tests run on tokio's paused clock: virtual time jumps to the next timer, so the
//! bounds below are exact and no test sleeps for real.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_mtproto::auth_key::{AuthKeyType, Dcenter, ManagedAuthKey};
use tg_mtproto::crypto::{AuthKey, Side};
use tg_mtproto::dc_options::{DcOptions, Environment};
use tg_mtproto::message::{
    build_encrypted_packet, next_request_id, parse_encrypted_packet, unpack_container,
};
use tg_mtproto::receive_watchdog::{MAX_RECEIVE_TIMEOUT, MIN_RECEIVE_TIMEOUT};
use tg_mtproto::session::{
    CHECK_SENT_REQUEST_TIMEOUT, PING_SEND_AFTER_FORCE, QueuedRequest, SessionHandle,
};
use tg_mtproto::tl::mtproto::{
    CoreMessage, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_mtproto::tl::{
    Boxed, Deserializable, Identifiable, RawObject, Serializable, enums as tl_enums,
    functions as tl_functions,
};
use tg_mtproto::transport_adapter::FakeServerPeer;
use tg_mtproto::{ClientConfig, FakeTransportFactory, MtprotoClock};
use tokio::time::Instant;

const DUMMY_CALL: u32 = 0x1234_5678;
const DUMMY_REPLY: u32 = 0x8765_4321;
const SERVER_SALT: u64 = 0x7777_8888;

fn key_data(seed: u8) -> AuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(29).wrapping_add(seed);
    }
    AuthKey::new(data)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Answers pings, requests and every content message.
    Healthy,
    /// Answers pings only; after the first request it holds the connection open and says
    /// nothing. It acknowledges nothing, so no late packet can arrive after the request.
    SilentAfterRpc,
    /// Acknowledges the first request (delivered, result pending), then says nothing.
    AckRpcThenSilent,
    /// Never answers anything (a black-holed connection).
    Silent,
    /// Answers the first ping only, then keeps sending updates but no pongs.
    PongOnceThenUpdates,
    /// Answers a request after 20 s while its bytes trickle in every 2 s.
    SlowReply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    FirstPacket,
    Ping,
    Rpc(u32),
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<(usize, Seen, Instant)>>>);

impl Log {
    fn push(&self, peer: usize, seen: Seen) {
        self.0
            .lock()
            .expect("log")
            .push((peer, seen, Instant::now()));
    }

    fn entries(&self) -> Vec<(usize, Seen, Instant)> {
        self.0.lock().expect("log").clone()
    }

    fn first_packet_at(&self, peer: usize) -> Option<Instant> {
        self.entries()
            .into_iter()
            .find(|(p, seen, _)| *p == peer && *seen == Seen::FirstPacket)
            .map(|(_, _, at)| at)
    }

    fn rpcs(&self, tag: u32) -> Vec<(usize, Instant)> {
        self.entries()
            .into_iter()
            .filter(|(_, seen, _)| *seen == Seen::Rpc(tag))
            .map(|(peer, _, at)| (peer, at))
            .collect()
    }

    fn count(&self, seen: &Seen) -> usize {
        self.entries().iter().filter(|(_, s, _)| s == seen).count()
    }
}

/// Server-side message ids shared by all connections, so they keep growing across reconnects.
#[derive(Clone, Default)]
struct ServerIds(Arc<AtomicU64>);

impl ServerIds {
    fn next(&self, content: bool) -> (u64, u32) {
        let n = self.0.fetch_add(1, Ordering::AcqRel) + 1;
        let secs = tg_mtproto::time::local_unix_secs() as u64;
        let msg_id = (secs << 32) | (n << 2) | if content { 3 } else { 1 };
        let seq_no = (n as u32) * 2 + u32::from(content);
        (msg_id, seq_no)
    }
}

fn unwrap_inner_query(body: &[u8]) -> Vec<u8> {
    if let Ok(wrapped) =
        tl_functions::InvokeWithLayer::<tl_functions::InitConnection<RawObject>>::from_bytes(body)
    {
        return wrapped.query.query.0;
    }
    body.to_vec()
}

fn ctor(body: &[u8]) -> u32 {
    body.get(0..4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn messages(raw: &[u8], key: &AuthKey) -> Option<(u64, Vec<CoreMessage>)> {
    let env = parse_encrypted_packet(raw, key, Side::Client).ok()?;
    let session_id = env.session_id;
    if ctor(&env.body) == MsgContainer::CONSTRUCTOR_ID {
        Some((session_id, unpack_container(&env.body).ok()?))
    } else {
        Some((
            session_id,
            vec![CoreMessage {
                msg_id: env.msg_id as i64,
                seqno: env.seq_no as i32,
                body: RawObject(env.body),
            }],
        ))
    }
}

fn ping_id(body: &[u8]) -> Option<i64> {
    match ctor(body) {
        id if id == mtp_functions::Ping::CONSTRUCTOR_ID => mtp_functions::Ping::from_bytes(body)
            .ok()
            .map(|p| p.ping_id),
        id if id == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID => {
            mtp_functions::PingDelayDisconnect::from_bytes(body)
                .ok()
                .map(|p| p.ping_id)
        }
        _ => None,
    }
}

struct Server {
    peer: FakeServerPeer,
    index: usize,
    key: AuthKey,
    mode: Mode,
    log: Log,
    ids: ServerIds,
    session_id: u64,
}

impl Server {
    fn send(&self, content: bool, body: &[u8]) {
        let (msg_id, seq_no) = self.ids.next(content);
        let packet = build_encrypted_packet(
            SERVER_SALT,
            self.session_id,
            msg_id,
            seq_no,
            body,
            &self.key,
            Side::Server,
        );
        let _ = self.peer.tx.send(Ok(packet));
    }

    async fn run(mut self) {
        let mut silent = self.mode == Mode::Silent;
        let mut pongs = 0usize;
        let mut first = true;
        let mut updates = tokio::time::interval(Duration::from_secs(2));
        updates.tick().await;
        loop {
            let streaming = self.mode == Mode::PongOnceThenUpdates && pongs > 0;
            let raw = tokio::select! {
                raw = self.peer.rx.recv() => match raw {
                    Some(raw) => raw,
                    None => return,
                },
                _ = updates.tick(), if streaming => {
                    self.send(true, &tl_enums::Updates::TooLong.to_bytes());
                    continue;
                }
            };
            if first {
                first = false;
                self.log.push(self.index, Seen::FirstPacket);
            }
            if silent {
                continue;
            }
            let Some((session_id, msgs)) = messages(&raw, &self.key) else {
                continue;
            };
            self.session_id = session_id;
            for m in msgs {
                let inner = unwrap_inner_query(&m.body.0);
                if let Some(ping_id) = ping_id(&inner) {
                    self.log.push(self.index, Seen::Ping);
                    if self.mode == Mode::PongOnceThenUpdates && pongs > 0 {
                        continue;
                    }
                    pongs += 1;
                    let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                        msg_id: m.msg_id,
                        ping_id,
                    });
                    self.send(false, &pong.to_bytes());
                } else if ctor(&inner) == DUMMY_CALL {
                    let tag = u32::from_le_bytes(inner[4..8].try_into().unwrap_or([0; 4]));
                    self.log.push(self.index, Seen::Rpc(tag));
                    match self.mode {
                        Mode::SilentAfterRpc => {
                            silent = true;
                            break;
                        }
                        Mode::AckRpcThenSilent => {
                            let ack = mtp_enums::MsgsAck::MsgsAck(mtp_types::MsgsAck {
                                msg_ids: vec![m.msg_id],
                            });
                            self.send(false, &ack.to_bytes());
                            silent = true;
                            break;
                        }
                        Mode::SlowReply => {
                            for _ in 0..10 {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                self.peer.deliver_partial(4096);
                            }
                        }
                        _ => {}
                    }
                    let mut reply = DUMMY_REPLY.to_le_bytes().to_vec();
                    reply.extend_from_slice(&(tag * 10).to_le_bytes());
                    let result = Boxed(RpcResult {
                        req_msg_id: m.msg_id,
                        result: RawObject(reply),
                    });
                    self.send(true, &result.to_bytes());
                } else if m.seqno & 1 == 1 && self.mode != Mode::SilentAfterRpc {
                    // Any other content message (e.g. get_future_salts, msgs_state_req).
                    let ack = mtp_enums::MsgsAck::MsgsAck(mtp_types::MsgsAck {
                        msg_ids: vec![m.msg_id],
                    });
                    self.send(false, &ack.to_bytes());
                }
            }
        }
    }
}

struct Harness {
    factory: Arc<FakeTransportFactory>,
    log: Log,
    session: SessionHandle,
    _updates_rx: tokio::sync::mpsc::UnboundedReceiver<tg_mtproto::session::UpdatesMsg>,
}

/// Spawns a main-DC session over fake connections served in `modes` order. With `with_key`
/// false the DC has no auth key yet, so each connection starts with DH key creation.
async fn harness(modes: &[Mode], ping_interval: Duration, with_key: bool) -> Harness {
    let key = key_data(7);
    let factory = Arc::new(FakeTransportFactory::new());
    let log = Log::default();
    let ids = ServerIds::default();
    for (index, &mode) in modes.iter().enumerate() {
        let peer = factory.push_connection().await;
        let server = Server {
            peer,
            index,
            key: key.clone(),
            mode,
            log: log.clone(),
            ids: ids.clone(),
            session_id: 0,
        };
        tokio::spawn(server.run());
    }
    let managed = with_key.then(|| ManagedAuthKey::new(key, AuthKeyType::ReadFromFile, 2));
    let dcenter = Dcenter::new(2, managed);
    let (updates_tx, updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let config = ClientConfig {
        use_pfs: false,
        ping_interval,
        ..ClientConfig::default()
    };
    let session = SessionHandle::spawn(
        2,
        config,
        DcOptions::new(Environment::Production),
        MtprotoClock::new(),
        dcenter,
        factory.clone(),
        updates_tx,
    );
    Harness {
        factory,
        log,
        session,
        _updates_rx: updates_rx,
    }
}

fn dummy_request(tag: u32) -> QueuedRequest {
    let mut body = DUMMY_CALL.to_le_bytes().to_vec();
    body.extend_from_slice(&tag.to_le_bytes());
    let mut req = QueuedRequest::new(next_request_id(), body);
    req.needs_layer = false;
    req
}

fn reply_value(reply: &[u8]) -> Option<u32> {
    (ctor(reply) == DUMMY_REPLY)
        .then(|| reply.get(4..8)?.try_into().ok().map(u32::from_le_bytes))
        .flatten()
}

async fn wait_until(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

const SLACK: Duration = Duration::from_millis(50);

/// A request sent into a connection that stops answering: the session dials again after the
/// 4 s window and sends the request on the new connection exactly once. Before, it waited for
/// the OS to fail the socket and the request hung.
#[tokio::test(start_paused = true)]
async fn silent_connection_is_restarted_and_the_request_resent_once() {
    let h = harness(
        &[Mode::SilentAfterRpc, Mode::Healthy],
        Duration::from_secs(300),
        true,
    )
    .await;
    let session = h.session.clone();
    let rpc = tokio::spawn(async move { session.send_raw(dummy_request(9)).await });

    let Ok(reply) = tokio::time::timeout(Duration::from_secs(120), rpc).await else {
        panic!(
            "the request was not answered; connects {}, server saw {:?}",
            h.factory.connect_calls(),
            h.log.entries()
        );
    };
    let reply = reply.expect("rpc task").expect("rpc ok");
    assert_eq!(reply_value(&reply), Some(90));

    let rpcs = h.log.rpcs(9);
    assert_eq!(rpcs.len(), 2, "sent once per connection: {rpcs:?}");
    assert_eq!((rpcs[0].0, rpcs[1].0), (0, 1));
    let reconnect_at = h.log.first_packet_at(1).expect("second connection used");
    let waited = reconnect_at - rpcs[0].1;
    assert!(
        waited >= MIN_RECEIVE_TIMEOUT && waited <= MIN_RECEIVE_TIMEOUT + SLACK,
        "restarted after {waited:?}"
    );
    assert_eq!(h.factory.connect_calls(), 2);

    // Nothing is sent a third time later.
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(h.log.rpcs(9).len(), 2);
    assert_eq!(h.factory.connect_calls(), 2);
    h.session.shutdown();
}

/// The server acknowledges the request and then goes silent. The ack is data received, so
/// it cancels the receive window; like tdesktop, the state check 10 s after the send
/// (`msgs_state_req` needs a reply) opens a new window, and its 4 s run out.
#[tokio::test(start_paused = true)]
async fn acknowledged_request_without_answer_restarts_after_the_state_check() {
    let h = harness(
        &[Mode::AckRpcThenSilent, Mode::Healthy],
        Duration::from_secs(300),
        true,
    )
    .await;
    let session = h.session.clone();
    let rpc = tokio::spawn(async move { session.send_raw(dummy_request(5)).await });
    let Ok(reply) = tokio::time::timeout(Duration::from_secs(120), rpc).await else {
        panic!(
            "the request was not answered; connects {}, server saw {:?}",
            h.factory.connect_calls(),
            h.log.entries()
        );
    };
    let reply = reply.expect("rpc task").expect("rpc ok");
    assert_eq!(reply_value(&reply), Some(50));

    let rpcs = h.log.rpcs(5);
    assert_eq!(rpcs.len(), 2, "sent once per connection: {rpcs:?}");
    let reconnect_at = h.log.first_packet_at(1).expect("second connection used");
    let waited = reconnect_at - rpcs[0].1;
    let expected = CHECK_SENT_REQUEST_TIMEOUT + MIN_RECEIVE_TIMEOUT;
    assert!(
        waited >= expected && waited <= expected + SLACK,
        "restarted after {waited:?}"
    );
    h.session.shutdown();
}

/// An idle connection whose server answers every ping is never restarted.
#[tokio::test(start_paused = true)]
async fn healthy_idle_connection_is_not_restarted() {
    let h = harness(&[Mode::Healthy], Duration::from_secs(30), true).await;
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(h.factory.connect_calls(), 1);
    // The first ping on connect plus one every 30 s.
    assert!(h.log.count(&Seen::Ping) >= 20, "{:?}", h.log.entries());

    // And a request on it still works.
    let reply = tokio::time::timeout(Duration::from_secs(5), h.session.send_raw(dummy_request(4)))
        .await
        .expect("answered")
        .expect("rpc ok");
    assert_eq!(reply_value(&reply), Some(40));
    assert_eq!(h.factory.connect_calls(), 1);
    h.session.shutdown();
}

/// A large reply that arrives slowly is not a silent connection: tdesktop's watchdog is
/// cancelled by every socket read, and ours by the transport's received-bytes counter.
#[tokio::test(start_paused = true)]
async fn slow_reply_still_arriving_does_not_restart() {
    let h = harness(&[Mode::SlowReply], Duration::from_secs(300), true).await;
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        h.session.send_raw(dummy_request(3)),
    )
    .await
    .expect("answered")
    .expect("rpc ok");
    assert_eq!(reply_value(&reply), Some(30));
    assert_eq!(h.factory.connect_calls(), 1);
    h.session.shutdown();
}

/// The server keeps sending updates but stops answering pings: tdesktop restarts when a ping
/// has no pong 45 s after it was sent. The keepalive goes out 30 s after connecting.
#[tokio::test(start_paused = true)]
async fn ping_without_pong_restarts_after_45_seconds() {
    let h = harness(
        &[Mode::PongOnceThenUpdates, Mode::Healthy],
        Duration::from_secs(30),
        true,
    )
    .await;
    wait_until("the second connection", Duration::from_secs(300), || {
        h.log.first_packet_at(1).is_some()
    })
    .await;
    let connected_at = h.log.first_packet_at(0).expect("first connection");
    let restarted_at = h.log.first_packet_at(1).expect("second connection");
    let expected = Duration::from_secs(30) + PING_SEND_AFTER_FORCE;
    let waited = restarted_at - connected_at;
    assert!(
        waited >= expected && waited <= expected + SLACK,
        "restarted after {waited:?}"
    );
    h.session.shutdown();
}

/// Auth key creation on a silent connection: the DH request gets the 4 s window, the next
/// attempt 8 s (the window doubles after each miss, up to 64 s). Before, DH waited forever.
#[tokio::test(start_paused = true)]
async fn silent_key_creation_is_retried_with_a_doubling_window() {
    let h = harness(
        &[Mode::Silent, Mode::Silent, Mode::Silent],
        Duration::from_secs(300),
        false,
    )
    .await;
    wait_until("the third connection", Duration::from_secs(120), || {
        h.log.first_packet_at(2).is_some()
    })
    .await;
    let at: Vec<Instant> = (0..3)
        .map(|i| h.log.first_packet_at(i).expect("connection used"))
        .collect();
    let first = at[1] - at[0];
    let second = at[2] - at[1];
    assert!(
        first >= MIN_RECEIVE_TIMEOUT && first <= MIN_RECEIVE_TIMEOUT + SLACK,
        "first retry after {first:?}"
    );
    let doubled = MIN_RECEIVE_TIMEOUT * 2;
    assert!(
        second >= doubled && second <= doubled + SLACK,
        "second retry after {second:?}"
    );
    assert!(second < MAX_RECEIVE_TIMEOUT);
    h.session.shutdown();
}
