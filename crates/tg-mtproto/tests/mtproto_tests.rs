//! Unit, protocol, in-memory async integration, and `#[ignore]` live DC2 tests for `tg-mtproto`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tg_mtproto::auth_key::{
    AuthKeyType, DcKeyBindState, Dcenter, ManagedAuthKey, handle_bind_temp_key_response,
    prepare_bind_temp_key_request,
};
use tg_mtproto::config::{Config, ConfigFields};
use tg_mtproto::config_loader::{
    ConfigLoader, ConfigLoaderOptions, compute_config_refresh_delay, next_enum_dc,
};
use tg_mtproto::crypto::{
    AuthKey, Side, aes_cbc_encrypt, aes_ige_decrypt, aes_ige_encrypt, decrypt_data_v2, dh,
    encrypt_data_v2, factorize, factorize_bytes, rsa, sha256,
};
use tg_mtproto::dc_options::{
    BUILT_IN_RSA_KEYS_PRODUCTION, BUILT_IN_RSA_KEYS_TEST, DcEndpoint, DcOptionFlags, DcOptions,
    DcType, Environment, bare_dc_id, destroy_key_next_dc_id, download_dc_id, get_dc_id_shift,
    is_download_dc_id, is_media_cluster_dc_id, protocol_dc_id, shift_dc_id,
    temporary_dc_id_from_real,
};
use tg_mtproto::file_loader::DedicatedFileDownloader;
use tg_mtproto::keys_destroyer::{KeyDestroyOutcome, KeysDestroyer};
use tg_mtproto::message::{
    CUT_CONTAINER_ON_BYTES, DecryptedEnvelope, MAX_GZIP_NESTING, build_encrypted_packet,
    gzip_pack_if_smaller, gzip_unpack, need_ack_for_constructor, pack_container,
    parse_encrypted_packet, unpack_container, unwrap_gzip_layers, wrap_init_connection,
    wrap_invoke_after_msg,
};
use tg_mtproto::received_ids::{IDS_BUFFER_SIZE, ReceivedIdsManager, RegisterIdResult};
use tg_mtproto::salts::SaltManager;
use tg_mtproto::session::{
    CHECK_SENT_REQUEST_TIMEOUT, CheckSentOutcome, HTTP_WAIT_MAX_DELAY_MS, HTTP_WAIT_MAX_WAIT_MS,
    HTTP_WAIT_WAIT_AFTER_MS, QueuedRequest, SENT_CONTAINER_LIVES, SessionAction, SessionEvent,
    SessionState, next_retry_backoff_ms,
};
use tg_mtproto::special_config::{
    DNS_TYPE_TXT, DnsEntry, SpecialConfigError, SpecialEndpoint, check_phone_by_prefixes_rules,
    concatenate_dns_txt_fields, decrypt_simple_config, decrypt_simple_config_rsa_block,
    extract_special_endpoints, generate_dns_random_padding, parse_dns_response, parse_http_date,
};
use tg_mtproto::tl::mtproto::{
    CoreMessage, GzipPacked, MsgContainer, RpcResult, enums as mtp_enums,
    functions as mtp_functions, types as mtp_types,
};
use tg_mtproto::tl::{
    BOOL_TRUE_ID, Boxed, Cursor, Deserializable, DeserializeError, Identifiable, LAYER, RawObject,
    RemoteCall, Serializable, enums as tl_enums, functions as tl_functions, types as tl_types,
};
use tg_mtproto::transport::{
    Abridged, FrameKind, Intermediate, Obfuscated2, PaddedIntermediate, ProxyData, RaceEndpoint,
    TAG_INTERMEDIATE, Transport, TransportErrorCode, TransportKind,
};
use tg_mtproto::{
    Client, ClientConfig, FakeTransportFactory, InvocationError, MigrateKind, MtprotoClock,
    RpcClient, RpcError, TransportError, build_race_candidates,
};

// ============================================================================
// Helpers for unit tests
// ============================================================================

fn make_dc_option(
    id: i32,
    ip_address: &str,
    port: i32,
    media_only: bool,
    is_static: bool,
) -> tl_enums::DcOption {
    tl_enums::DcOption::DcOption(tl_types::DcOption {
        ipv6: false,
        media_only,
        tcpo_only: false,
        cdn: false,
        r#static: is_static,
        this_port_only: false,
        id,
        ip_address: ip_address.to_string(),
        port,
        secret: None,
    })
}

fn boxed_rpc_error(code: i32, message: &str) -> Vec<u8> {
    mtp_enums::RpcError::RpcError(mtp_types::RpcError {
        error_code: code,
        error_message: message.to_string(),
    })
    .to_bytes()
}

fn boxed_pong(msg_id: i64, ping_id: i64) -> Vec<u8> {
    mtp_enums::Pong::Pong(mtp_types::Pong { msg_id, ping_id }).to_bytes()
}

fn boxed_rpc_result(req_msg_id: i64, result: Vec<u8>) -> Vec<u8> {
    Boxed(RpcResult {
        req_msg_id,
        result: RawObject(result),
    })
    .to_bytes()
}

fn test_auth_key(seed: u8) -> AuthKey {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(31));
    }
    AuthKey::new(data)
}

fn make_server_envelope(
    state: &SessionState,
    salt: u64,
    server_time: i32,
    counter: u64,
    seq_no: u32,
    body: Vec<u8>,
) -> DecryptedEnvelope {
    // Server reply msg_id has (msg_id & 3) == 1.
    let msg_id = ((server_time as u32 as u64) << 32) | ((counter << 2) | 1);
    DecryptedEnvelope {
        server_salt: salt,
        session_id: state.session_id(),
        msg_id,
        seq_no,
        body,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DummyCall {
    tag: u32,
}

impl Serializable for DummyCall {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        buf.extend_from_slice(&self.tag.to_le_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DummyReply {
    value: u32,
}

impl Serializable for DummyReply {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&0x8765_4321u32.to_le_bytes());
        buf.extend_from_slice(&self.value.to_le_bytes());
    }
}

impl Deserializable for DummyReply {
    fn deserialize(buf: &mut Cursor<'_>) -> Result<Self, DeserializeError> {
        let ctor = buf.read_u32()?;
        if ctor != 0x8765_4321 {
            return Err(DeserializeError::UnexpectedConstructor { id: ctor });
        }
        Ok(Self {
            value: buf.read_u32()?,
        })
    }
}

impl RemoteCall for DummyCall {
    type Return = DummyReply;
}

// ============================================================================
// 1. msg_id / seq_no / time offset tests
// ============================================================================

#[test]
fn msg_id_is_monotonic_time_based_and_divisible_by_4() {
    let clock = MtprotoClock::new();
    let base_secs = clock.now_secs() as u64;
    let mut prev = 0u64;
    for _ in 0..500 {
        let id = clock.next_msg_id();
        assert_eq!(id % 4, 0, "client msg_id must be divisible by 4");
        assert!(id > prev, "msg_id must be strictly increasing");
        let id_secs = id >> 32;
        assert!(
            id_secs >= base_secs.saturating_sub(1) && id_secs <= base_secs + 5,
            "msg_id upper 32 bits must encode unix time"
        );
        prev = id;
    }
}

#[test]
fn server_time_offset_correction_and_threshold() {
    let clock = MtprotoClock::new();
    let local = clock.now_secs();
    // Small diff (< 3s) ignored when force == false.
    assert!(!clock.update_server_time(local + 2, false));
    assert_eq!(clock.time_shift_secs(), 0);

    // Large diff (+120s) applied on first non-forced update.
    assert!(clock.update_server_time(local + 120, false));
    assert!((clock.time_shift_secs() - 120).abs() <= 1);

    // Subsequent non-forced update is ignored (matches base::unixtime::update).
    assert!(!clock.update_server_time(local + 500, false));
    assert!((clock.time_shift_secs() - 120).abs() <= 1);

    // Forced update overrides offset and re-anchors msg_id.
    assert!(clock.update_server_time(local - 400, true));
    assert!((clock.time_shift_secs() + 400).abs() <= 1);
    let id = clock.next_msg_id();
    assert_eq!(id % 4, 0);
    let id_secs = (id >> 32) as i64;
    assert!((id_secs - (local as i64 - 400)).abs() <= 2);
}

#[test]
fn seq_no_rules_for_content_and_non_content_messages() {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock,
        dcenter,
        0x1111,
    );

    // Non-content before any content -> 0, counter stays 0.
    assert_eq!(state.next_seq_no(false), 0);
    assert_eq!(state.messages_counter(), 0);
    // First content -> 1, counter becomes 1.
    assert_eq!(state.next_seq_no(true), 1);
    assert_eq!(state.messages_counter(), 1);
    // Non-content -> 2, counter stays 1.
    assert_eq!(state.next_seq_no(false), 2);
    assert_eq!(state.messages_counter(), 1);
    // Second content -> 3, counter becomes 2.
    assert_eq!(state.next_seq_no(true), 3);
    assert_eq!(state.messages_counter(), 2);

    assert!(!need_ack_for_constructor(MsgContainer::CONSTRUCTOR_ID));
    assert!(!need_ack_for_constructor(
        mtp_types::MsgsAck::CONSTRUCTOR_ID
    ));
    assert!(need_ack_for_constructor(0x1234_5678));
}

// ============================================================================
// 2. Container packing / unpacking & splitting tests
// ============================================================================

#[test]
fn container_pack_unpack_roundtrip_and_splitting() {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock,
        dcenter,
        0xCAFE_BABE,
    );

    // Enqueue 3 normal requests -> should pack into one msg_container.
    for i in 1..=3u64 {
        state.enqueue_request(QueuedRequest::new(
            i,
            DummyCall { tag: i as u32 }.to_bytes(),
        ));
    }
    let out = state.build_next_outbound().expect("outbound container");
    assert!(!out.has_more);
    assert_eq!(out.seq_no % 2, 0, "container seq_no must be even");
    let items = unpack_container(&out.body).expect("valid container");
    assert_eq!(items.len(), 3);
    for item in &items {
        let item_msg_id = item.msg_id as u64;
        assert_eq!(item_msg_id % 4, 0);
        assert!(
            item_msg_id < out.msg_id,
            "inner msg_id must be < container msg_id"
        );
        assert_eq!((item.seqno as u32) % 2, 1, "RPC inner seq_no must be odd");
    }
    assert_eq!(state.have_sent_len(), 3);

    // Enqueue two large requests exceeding CUT_CONTAINER_ON_BYTES -> splits across 2 packets.
    let big_body = vec![0xABu8; CUT_CONTAINER_ON_BYTES / 2 + 1024];
    state.enqueue_request(QueuedRequest::new(10, big_body.clone()));
    state.enqueue_request(QueuedRequest::new(11, big_body.clone()));
    state.enqueue_request(QueuedRequest::new(12, big_body));

    let first = state.build_next_outbound().expect("first split batch");
    assert!(first.has_more, "remaining requests should trigger has_more");
    let second = state.build_next_outbound().expect("second split batch");
    assert!(!second.has_more);
}

#[test]
fn container_unpack_rejects_unaligned_and_truncated() {
    let mut bad = Vec::new();
    bad.extend_from_slice(&MsgContainer::CONSTRUCTOR_ID.to_le_bytes());
    bad.extend_from_slice(&1u32.to_le_bytes());
    bad.extend_from_slice(&100u64.to_le_bytes());
    bad.extend_from_slice(&1u32.to_le_bytes());
    bad.extend_from_slice(&5u32.to_le_bytes()); // not multiple of 4
    bad.extend_from_slice(&[0u8; 5]);
    assert!(unpack_container(&bad).is_err());

    let packed = pack_container(vec![CoreMessage {
        msg_id: 100,
        seqno: 1,
        body: RawObject(mtp_functions::Ping { ping_id: 42 }.to_bytes()),
    }]);
    assert!(unpack_container(&packed[..packed.len() - 2]).is_err());
}

// ============================================================================
// 3. Ack, resend, bad_msg_notification, bad_server_salt, state_info tests
// ============================================================================

#[test]
fn ack_and_resend_bookkeeping_lifecycle() {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock.clone(),
        dcenter,
        0x55AA,
    );

    // Enqueue 2 requests -> packed into container.
    state.enqueue_request(QueuedRequest::new(1, DummyCall { tag: 1 }.to_bytes()));
    state.enqueue_request(QueuedRequest::new(2, DummyCall { tag: 2 }.to_bytes()));
    let out = state.build_next_outbound().expect("container");
    let items = unpack_container(&out.body).expect("unpack");
    let m1 = items[0].msg_id;
    let m2 = items[1].msg_id;
    let container_id = out.msg_id;
    assert_eq!(state.have_sent_len(), 2);

    // Receiving msgs_ack for container_id keeps RPC-expecting requests in have_sent until rpc_result.
    let ack_env = make_server_envelope(
        &state,
        0x55AA,
        clock.now_secs(),
        1,
        0,
        mtp_enums::MsgsAck::MsgsAck(mtp_types::MsgsAck {
            msg_ids: vec![container_id as i64],
        })
        .to_bytes(),
    );
    let res = state.handle_decrypted(ack_env);
    assert_eq!(state.have_sent_len(), 2);
    assert!(!res.has_pending_acks);

    // Server sends bad_server_salt for m1 -> updates salt and moves m1 back to to_send.
    let bad_salt_env = make_server_envelope(
        &state,
        0x55AA,
        clock.now_secs(),
        2,
        0,
        mtp_enums::BadMsgNotification::BadServerSalt(mtp_types::BadServerSalt {
            bad_msg_id: m1,
            bad_msg_seqno: items[0].seqno,
            error_code: 48,
            new_server_salt: 0xDEAD_BEEFu64 as i64,
        })
        .to_bytes(),
    );
    let res = state.handle_decrypted(bad_salt_env);
    assert_eq!(state.salts().active_salt(), 0xDEAD_BEEF);
    assert_eq!(res.action, Some(SessionAction::FlushOutbound));
    assert_eq!(state.to_send_len(), 1);
    assert_eq!(state.have_sent_len(), 1);

    // Resend packet uses new salt and forces container wrapping (matching tdesktop).
    let resent = state.build_next_outbound().expect("resent packet");
    assert_eq!(resent.salt, 0xDEAD_BEEF);
    let resent_items = unpack_container(&resent.body).expect("forced container");
    assert_eq!(resent_items.len(), 1);

    // Server sends msg_resend_req for m2 -> moves m2 back to to_send.
    let resend_req_env = make_server_envelope(
        &state,
        0xDEAD_BEEF,
        clock.now_secs(),
        3,
        1, // content-related -> should queue an ack!
        mtp_enums::MsgResendReq::MsgResendReq(mtp_types::MsgResendReq { msg_ids: vec![m2] })
            .to_bytes(),
    );
    let res = state.handle_decrypted(resend_req_env);
    assert!(res.has_pending_acks);
    assert_eq!(state.pending_ack_len(), 1);
    assert_eq!(state.to_send_len(), 1);

    // Next outbound carries both the resent m2 and msgs_ack!
    let next_out = state.build_next_outbound().expect("m2 + ack");
    let next_items = unpack_container(&next_out.body).expect("container");
    assert_eq!(next_items.len(), 2);
    assert_eq!(state.pending_ack_len(), 0);
}

#[test]
fn bad_msg_notification_codes_and_state_sync() {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock.clone(),
        dcenter,
        0x1234,
    );

    // Send 1 request.
    state.enqueue_request(QueuedRequest::new(7, DummyCall { tag: 7 }.to_bytes()));
    let out = state.build_next_outbound().expect("single");
    let m1 = out.msg_id;

    // Code 16 (msg_id too low) -> updates time & queues resend.
    let env16 = make_server_envelope(
        &state,
        0x1234,
        clock.now_secs() + 120,
        1,
        0,
        mtp_enums::BadMsgNotification::BadMsgNotification(mtp_types::BadMsgNotification {
            bad_msg_id: m1 as i64,
            bad_msg_seqno: out.seq_no as i32,
            error_code: 16,
        })
        .to_bytes(),
    );
    let res = state.handle_decrypted(env16);
    assert_eq!(res.action, Some(SessionAction::FlushOutbound));
    assert_eq!(state.to_send_len(), 1);

    // Re-send it, then test code 32 -> must trigger ResetSession.
    let out2 = state.build_next_outbound().expect("resent");
    let inner = unpack_container(&out2.body).expect("container");
    let m1_new = inner[0].msg_id;
    let env32 = make_server_envelope(
        &state,
        0x1234,
        clock.now_secs(),
        2,
        0,
        mtp_enums::BadMsgNotification::BadMsgNotification(mtp_types::BadMsgNotification {
            bad_msg_id: m1_new,
            bad_msg_seqno: inner[0].seqno,
            error_code: 32,
        })
        .to_bytes(),
    );
    let res32 = state.handle_decrypted(env32);
    assert_eq!(res32.action, Some(SessionAction::ResetSession));

    // Reset session changes session_id, resets counter, and re-queues in-flight requests.
    let old_sid = state.session_id();
    state.reset_session();
    assert_ne!(state.session_id(), old_sid);
    assert_eq!(state.messages_counter(), 0);
    assert_eq!(state.to_send_len(), 1);

    // Send again, then simulate timeout -> check_sent_requests queues msgs_state_req.
    let out3 = state.build_next_outbound().expect("after reset");
    let m3 = unpack_container(&out3.body).expect("container")[0].msg_id;
    let future = Instant::now() + CHECK_SENT_REQUEST_TIMEOUT + Duration::from_secs(1);
    assert_eq!(
        state.check_sent_requests(future),
        CheckSentOutcome::QueuedStateRequests
    );
    let state_req_out = state.build_next_outbound().expect("msgs_state_req");
    let state_req_id = state_req_out.msg_id;

    // Server replies msgs_state_info with status byte 0x01 (not received) -> resends m3.
    let info_env = make_server_envelope(
        &state,
        0x1234,
        clock.now_secs(),
        3,
        0,
        mtp_enums::MsgsStateInfo::MsgsStateInfo(mtp_types::MsgsStateInfo {
            req_msg_id: state_req_id as i64,
            info: vec![0x01],
        })
        .to_bytes(),
    );
    let info_res = state.handle_decrypted(info_env);
    assert_eq!(info_res.action, Some(SessionAction::FlushOutbound));
    assert_eq!(state.to_send_len(), 1);
    let _ = m3;

    // Also verify clear_old_containers resends after SENT_CONTAINER_LIVES.
    let _ = state.build_next_outbound();
    assert!(
        state.clear_old_containers(Instant::now() + SENT_CONTAINER_LIVES + Duration::from_secs(1))
    );
    assert_eq!(state.to_send_len(), 1);
}

#[test]
fn new_session_created_updates_salt_resends_older_and_forwards_update() {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock.clone(),
        dcenter,
        0x1111,
    );

    state.enqueue_request(QueuedRequest::new(1, DummyCall { tag: 1 }.to_bytes()));
    let out1 = state.build_next_outbound().expect("req1");
    state.enqueue_request(QueuedRequest::new(2, DummyCall { tag: 2 }.to_bytes()));
    let out2 = state.build_next_outbound().expect("req2");

    // Server creates a new session starting at out2.msg_id -> out1 must be resent, out2 stays sent.
    let nsc_env = make_server_envelope(
        &state,
        0x9999,
        clock.now_secs(),
        1,
        1,
        mtp_enums::NewSession::Created(mtp_types::NewSessionCreated {
            first_msg_id: out2.msg_id as i64,
            unique_id: 777,
            server_salt: 0x9999,
        })
        .to_bytes(),
    );
    let outcome = state.handle_decrypted(nsc_env);
    assert_eq!(state.salts().active_salt(), 0x9999);
    assert_eq!(state.to_send_len(), 1);
    assert_eq!(state.have_sent_len(), 1);
    assert_eq!(outcome.events.len(), 1);
    assert!(matches!(outcome.events[0], SessionEvent::Update(_)));
    let _ = out1;
}

// ============================================================================
// 4. Salt handling & ReceivedIdsManager tests
// ============================================================================

#[test]
fn salt_manager_rotation_and_expiry() {
    let mut mgr = SaltManager::new(0x100);
    assert_eq!(mgr.current_salt(1000), 0x100);
    assert!(mgr.needs_refresh(1000));

    mgr.apply_future_salts(
        1000,
        vec![
            mtp_types::FutureSalt {
                valid_since: 900,
                valid_until: 1200,
                salt: 0x200,
            },
            mtp_types::FutureSalt {
                valid_since: 1200,
                valid_until: 2000,
                salt: 0x300,
            },
            mtp_types::FutureSalt {
                valid_since: 2000,
                valid_until: 3000,
                salt: 0x400,
            },
        ],
    );
    assert_eq!(mgr.current_salt(1000), 0x200);
    assert!(!mgr.needs_refresh(1000));
    // Within 60s expiry margin of 0x200 (1150 + 60 >= 1200), keeps current until 1200.
    assert_eq!(mgr.current_salt(1200), 0x300);
    // Past 2000 -> prunes earlier salts and rotates to 0x400.
    assert_eq!(mgr.current_salt(2050), 0x400);
    assert!(mgr.needs_refresh(2050));
}

#[test]
fn received_ids_manager_dedup_and_too_old() {
    let mut mgr = ReceivedIdsManager::new();
    assert_eq!(mgr.register_msg_id(101, true), RegisterIdResult::Success);
    assert_eq!(mgr.register_msg_id(101, true), RegisterIdResult::Duplicate);

    for i in 1..=(IDS_BUFFER_SIZE as u64) {
        assert_eq!(
            mgr.register_msg_id(1000 + i * 4 + 1, false),
            RegisterIdResult::Success
        );
    }
    mgr.shrink();
    assert_eq!(mgr.len(), IDS_BUFFER_SIZE);
    // Anything older than min() must return TooOld.
    assert_eq!(mgr.register_msg_id(501, true), RegisterIdResult::TooOld);
}

// ============================================================================
// 5. Error classification tests
// ============================================================================

#[test]
fn error_classification_migrate_flood_and_special_codes() {
    let e = RpcError::parse(303, "PHONE_MIGRATE_4");
    assert_eq!(e.migrate_dc(), Some((MigrateKind::Phone, 4)));
    assert_eq!(e.value, Some(4));

    assert_eq!(
        RpcError::parse(303, "FILE_MIGRATE_2").migrate_dc(),
        Some((MigrateKind::File, 2))
    );
    assert_eq!(
        RpcError::parse(303, "NETWORK_MIGRATE_1").migrate_dc(),
        Some((MigrateKind::Network, 1))
    );
    assert_eq!(
        RpcError::parse(303, "USER_MIGRATE_5").migrate_dc(),
        Some((MigrateKind::User, 5))
    );

    let flood = RpcError::parse(420, "FLOOD_WAIT_42: Too many requests");
    assert_eq!(flood.name, "FLOOD_WAIT_42");
    assert_eq!(flood.description, "Too many requests");
    assert_eq!(flood.flood_wait_secs(), Some(42));
    assert!(flood.is_flood());
    assert!(flood.is_temporary());
    assert!(flood.is_default_handled());

    let pflood = RpcError::parse(420, "FLOOD_PREMIUM_WAIT_15");
    assert_eq!(pflood.flood_premium_wait_secs(), Some(15));
    assert!(pflood.is_flood());

    let slow = RpcError::parse(420, "SLOWMODE_WAIT_2");
    assert_eq!(slow.slowmode_wait_secs(), Some(2));

    let temp_gone = RpcError::parse(401, "AUTH_KEY_PERM_EMPTY");
    assert!(temp_gone.is_destroyed_temp_key());
    assert!(!temp_gone.is_unauthorized());

    let unauth = RpcError::parse(401, "AUTH_KEY_UNREGISTERED");
    assert!(unauth.is_unauthorized());

    let bad_500 = RpcError::parse(502, "bad gateway html");
    assert_eq!(bad_500.name, "INTERNAL_SERVER_ERROR");
    assert!(bad_500.is_temporary());

    let bad_400 = RpcError::parse(400, "not a valid error!");
    assert_eq!(bad_400.name, "CLIENT_BAD_RPC_ERROR");

    assert!(RpcError::parse(400, "CONNECTION_NOT_INITED").is_connection_not_inited());
    assert!(RpcError::parse(400, "CONNECTION_LAYER_INVALID").is_connection_not_inited());
    assert!(RpcError::parse(400, "FILE_ID_INVALID").is_bad_guest_dc());
    assert!(RpcError::parse(406, "AUTH_KEY_DUPLICATED").is_ignored());
}

// ============================================================================
// 6. Crypto, gzip, framing, wrappers & DC options tests
// ============================================================================

#[test]
fn crypto_aes_ige_mtproto_v2_rsa_pq_and_dh_known_answers() {
    // AES-256-IGE roundtrip.
    let key = [0x42u8; 32];
    let iv = [0x13u8; 32];
    let pt = [0x5Au8; 64];
    let ct = aes_ige_encrypt(&pt, &key, &iv);
    assert_ne!(ct, pt);
    assert_eq!(aes_ige_decrypt(&ct, &key, &iv), pt);

    // MTProto 2.0 encrypt/decrypt roundtrip + tamper detection.
    let auth_key = test_auth_key(7);
    let body = mtp_functions::Ping { ping_id: 999 }.to_bytes();
    let pkt = build_encrypted_packet(
        0x1122,
        0x3344,
        0x5566_7788_0000_0000,
        1,
        &body,
        &auth_key,
        Side::Client,
    );
    let dec = parse_encrypted_packet(&pkt, &auth_key, Side::Client).expect("decrypt v2");
    assert_eq!(dec.server_salt, 0x1122);
    assert_eq!(dec.session_id, 0x3344);
    assert_eq!(dec.body, body);

    // Tampered ciphertext must fail msg_key check.
    let mut tampered = pkt.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;
    assert!(decrypt_data_v2(&tampered, &auth_key, Side::Client).is_err());

    // Direct encrypt_data_v2/decrypt_data_v2 (returns plaintext + trailing 12..=1024 padding).
    let mut plain = vec![0u8; 36];
    plain[28..32].copy_from_slice(&4u32.to_le_bytes());
    let wire = encrypt_data_v2(&plain, &auth_key, Side::Server);
    let decrypted_padded = decrypt_data_v2(&wire, &auth_key, Side::Server).unwrap();
    assert!(decrypted_padded.starts_with(&plain));

    // PQ factorization (known Telegram test vector: 0x17ED48941A08F981 = 0x494C553B * 0x53911073).
    let pq: u64 = 0x17ED_4894_1A08_F981;
    let (p, q) = factorize(pq);
    assert_eq!(p, 0x494C_553B);
    assert_eq!(q, 0x5391_1073);
    assert_eq!(p * q, pq);

    let (pb, qb) = factorize_bytes(&pq.to_be_bytes()).expect("factorize_bytes");
    assert_eq!(pb, 0x494C_553Bu32.to_be_bytes());
    assert_eq!(qb, 0x5391_1073u32.to_be_bytes());

    // Built-in production & test RSA keys parse cleanly and produce non-zero fingerprints & RSA_PAD.
    let prod_rsa = rsa::PublicKey::from_pem(BUILT_IN_RSA_KEYS_PRODUCTION[0]).expect("prod RSA key");
    assert_eq!(prod_rsa.n().len(), 256);
    assert_ne!(prod_rsa.fingerprint(), 0);
    let enc = prod_rsa.encrypt_hashed(&[1, 2, 3, 4]).expect("RSA_PAD");
    assert_eq!(enc.len(), 256);

    let test_rsa = rsa::PublicKey::from_pem(BUILT_IN_RSA_KEYS_TEST[0]).expect("test RSA key");
    assert_eq!(test_rsa.n().len(), 256);
    assert_ne!(test_rsa.fingerprint(), prod_rsa.fingerprint());

    // Telegram's built-in GOOD_PRIME check.
    assert!(dh::is_prime_and_good(&dh::GOOD_PRIME, 3));
    assert!(!dh::is_prime_and_good(&dh::GOOD_PRIME, 99));
}

#[test]
fn gzip_invoke_after_init_connection_and_bind_inner() {
    // Large compressible body gets gzip_packed.
    let big = vec![0xAAu8; 2048];
    let packed = gzip_pack_if_smaller(&big);
    assert!(packed.len() < big.len());
    assert_eq!(
        u32::from_le_bytes([packed[0], packed[1], packed[2], packed[3]]),
        GzipPacked::CONSTRUCTOR_ID
    );
    assert_eq!(gzip_unpack(&packed, 4096).unwrap(), big);
    // Exceeding size limit is rejected.
    assert!(gzip_unpack(&packed, 100).is_err());

    // Excessive gzip nesting is rejected.
    let mut nested = vec![0x11, 0x22, 0x33, 0x44];
    for _ in 0..=MAX_GZIP_NESTING {
        nested = Boxed(GzipPacked::pack(&nested).unwrap()).to_bytes();
    }
    let mut budget = 1024 * 1024;
    assert!(unwrap_gzip_layers(&nested, &mut budget, 0).is_err());

    // invokeAfterMsg wrapping roundtrips through generated InvokeAfterMsg<RawObject>.
    let inner = DummyCall { tag: 5 }.to_bytes();
    let after = wrap_invoke_after_msg(0xAABB_CCDD_0000_0000, &inner);
    let parsed_after =
        tl_functions::InvokeAfterMsg::<RawObject>::from_bytes(&after).expect("invokeAfterMsg");
    assert_eq!(parsed_after.msg_id as u64, 0xAABB_CCDD_0000_0000);
    assert_eq!(parsed_after.query.0, inner);

    // invokeWithLayer(initConnection(...)) uses caller-supplied config values (never hardcoded)
    // and decodes cleanly through generated InvokeWithLayer<InitConnection<RawObject>>.
    let cfg = ClientConfig {
        api_id: 987_654,
        device_model: "TestMachine".to_string(),
        system_version: "Win11".to_string(),
        app_version: "1.2.3".to_string(),
        system_lang_code: "en".to_string(),
        lang_pack: "tdesktop".to_string(),
        lang_code: "en".to_string(),
        tz_offset_secs: Some(7200),
        ..ClientConfig::default()
    };
    let wrapped = wrap_init_connection(&cfg, LAYER, false, &inner);
    let parsed_init =
        tl_functions::InvokeWithLayer::<tl_functions::InitConnection<RawObject>>::from_bytes(
            &wrapped,
        )
        .expect("invokeWithLayer(initConnection)");
    assert_eq!(parsed_init.layer, LAYER);
    assert_eq!(parsed_init.query.api_id, 987_654);
    assert_eq!(parsed_init.query.device_model, "TestMachine");
    assert_eq!(parsed_init.query.system_version, "Win11");
    assert_eq!(parsed_init.query.app_version, "1.2.3");
    assert!(parsed_init.query.proxy.is_none());
    assert!(parsed_init.query.params.is_some());
    assert_eq!(parsed_init.query.query.0, inner);

    // CDN redacts device_model & system_version to "n/a".
    let wrapped_cdn = wrap_init_connection(&cfg, LAYER, true, &inner);
    let parsed_cdn =
        tl_functions::InvokeWithLayer::<tl_functions::InitConnection<RawObject>>::from_bytes(
            &wrapped_cdn,
        )
        .expect("cdn initConnection");
    assert_eq!(parsed_cdn.query.device_model, "n/a");
    assert_eq!(parsed_cdn.query.system_version, "n/a");

    // PFS bindTempAuthKey request & response parsing.
    let clock = MtprotoClock::new();
    let perm = ManagedAuthKey::new(test_auth_key(1), AuthKeyType::Generated, 2);
    let mut temp = ManagedAuthKey::new(test_auth_key(2), AuthKeyType::Temporary, 2);
    temp.expires_at = Some(clock.now_secs() + 86_400);
    let (bind_msg_id, bind_body) =
        prepare_bind_temp_key_request(&perm, &temp, 0x7777, &clock).expect("prepare bind");
    assert_eq!(bind_msg_id % 4, 0);
    assert!(!bind_body.is_empty());
    assert_eq!(
        handle_bind_temp_key_response(&BOOL_TRUE_ID.to_le_bytes()),
        DcKeyBindState::Success
    );
    assert_eq!(
        handle_bind_temp_key_response(&boxed_rpc_error(400, "ENCRYPTED_MESSAGE_INVALID")),
        DcKeyBindState::DefinitelyDestroyed
    );
}

#[test]
fn dc_options_transports_and_backoff_sequence() {
    let opts = DcOptions::new(Environment::Production);
    assert_eq!(opts.config_enum_dc_ids(), vec![1, 2, 3, 4, 5]);
    let v2 = opts.lookup(2, DcType::Regular, false);
    assert!(!v2.ipv4_tcp.is_empty());
    assert!(!v2.ipv6_tcp.is_empty());

    // Shift helpers match tdesktop constants.
    let dl = download_dc_id(4, 2);
    assert_eq!(bare_dc_id(dl), 4);
    assert!(is_download_dc_id(dl));
    assert!(is_media_cluster_dc_id(dl));
    assert_eq!(get_dc_id_shift(shift_dc_id(2, 5)), 5);
    assert_eq!(protocol_dc_id(2, DcType::Regular, false), 2);
    assert_eq!(protocol_dc_id(2, DcType::MediaCluster, false), -2);
    assert_eq!(protocol_dc_id(2, DcType::Regular, true), 10_002);

    // Update with media_only option -> dcType becomes MediaCluster for download DC.
    let changed = opts.add_from_list(&[make_dc_option(4, "149.154.167.92", 443, true, true)]);
    assert_eq!(changed, vec![4]);
    assert_eq!(opts.dc_type(dl), DcType::MediaCluster);

    // Transport framing roundtrip (Intermediate, PaddedIntermediate & Abridged + Obfuscated2 from tg-transport).
    let payload = vec![0xDEu8; 512];
    let mut inter = Intermediate::new();
    let mut wire = Vec::new();
    inter.pack(&payload, &mut wire);
    let unpacked = inter.unpack(&wire).unwrap();
    assert_eq!(unpacked.kind, FrameKind::Payload);
    assert_eq!(unpacked.slice(&wire), payload.as_slice());

    let mut padded = PaddedIntermediate::new();
    let mut wire_p = Vec::new();
    padded.pack(&payload, &mut wire_p);
    let unpacked_p = padded.unpack(&wire_p).unwrap();
    assert_eq!(unpacked_p.kind, FrameKind::Payload);
    assert!(unpacked_p.slice(&wire_p).starts_with(&payload));

    let mut abr = Abridged::new();
    let mut wire2 = Vec::new();
    abr.pack(&payload, &mut wire2);
    let unpacked2 = abr.unpack(&wire2).unwrap();
    assert_eq!(unpacked2.kind, FrameKind::Payload);
    assert_eq!(unpacked2.slice(&wire2), payload.as_slice());

    let (mut obf, header) = Obfuscated2::generate(TAG_INTERMEDIATE, 2, None);
    assert_eq!(header.len(), 64);
    let mut sample = b"hello mtproto!!".to_vec();
    obf.encrypt_outgoing(&mut sample);
    assert_ne!(&sample, b"hello mtproto!!");

    // Reconnect backoff sequence: 1 -> 2 -> 3 -> 1000 -> 2000 -> ... -> 64000.
    let mut ms = 1;
    let mut seq = vec![ms];
    for _ in 0..10 {
        ms = next_retry_backoff_ms(ms);
        seq.push(ms);
    }
    assert_eq!(
        seq,
        vec![1, 2, 3, 1000, 2000, 4000, 8000, 16000, 32000, 64000, 64000]
    );

    // Dedicated file downloader out-of-order chunk reassembly & resume alignment.
    let mut dl_mgr = DedicatedFileDownloader::new(2, 1000, 350, 256).unwrap();
    assert_eq!(dl_mgr.progress().already, 256); // aligned down from 350 to 256
    let c1 = dl_mgr.next_chunk_request().unwrap();
    let c2 = dl_mgr.next_chunk_request().unwrap();
    assert_eq!(c1, (256, 256));
    assert_eq!(c2, (512, 256));
    // Receive c2 before c1 -> buffered until c1 arrives.
    assert!(
        dl_mgr
            .on_chunk_received(512, vec![2u8; 256])
            .unwrap()
            .is_empty()
    );
    let flushed = dl_mgr.on_chunk_received(256, vec![1u8; 256]).unwrap();
    assert_eq!(flushed.len(), 2);
    assert_eq!(flushed[0][0], 1);
    assert_eq!(flushed[1][0], 2);
    assert_eq!(dl_mgr.progress().already, 768);
}

// ============================================================================
// 7. End-to-end async Client & Session tests over in-memory FakeTransportFactory
// ============================================================================

/// Helper to strip `invokeWithLayer(initConnection(...))` using generated `tg-tl` types.
fn unwrap_inner_query(body: &[u8]) -> Vec<u8> {
    if let Ok(wrapped) =
        tl_functions::InvokeWithLayer::<tl_functions::InitConnection<RawObject>>::from_bytes(body)
    {
        return wrapped.query.query.0;
    }
    body.to_vec()
}

fn unpack_or_single(env: DecryptedEnvelope) -> Vec<CoreMessage> {
    if env.body.len() >= 4
        && u32::from_le_bytes(env.body[0..4].try_into().unwrap()) == MsgContainer::CONSTRUCTOR_ID
    {
        unpack_container(&env.body).unwrap()
    } else {
        vec![CoreMessage {
            msg_id: env.msg_id as i64,
            seqno: env.seq_no as i32,
            body: RawObject(env.body),
        }]
    }
}

#[tokio::test]
async fn async_client_invoke_gzip_reply_and_updates_forwarding() {
    let factory = Arc::new(FakeTransportFactory::new());
    let mut peer = factory.push_connection().await;

    let cfg = ClientConfig {
        api_id: 12345,
        use_pfs: false,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, mut updates_rx) = Client::with_transport(cfg, factory.clone());
    let key = test_auth_key(0x55);
    client.import_persistent_keys(vec![(2, key.clone())]);

    let server_key = key.clone();
    let server_task = tokio::spawn(async move {
        let salt = 0x7777_8888u64;
        let mut seq_counter = 0u32;
        let mut msg_counter = 1u64;
        let now_secs = tg_mtproto::time::local_unix_secs() as i32;

        while let Some(raw) = peer.rx.recv().await {
            let env =
                parse_encrypted_packet(&raw, &server_key, Side::Client).expect("server decrypt");
            let session_id = env.session_id;
            let msgs = unpack_or_single(env);

            for m in msgs {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    let pong = boxed_pong(m.msg_id, ping.ping_id);
                    let s_msg_id = ((now_secs as u64) << 32) | ((msg_counter << 2) | 1);
                    msg_counter += 1;
                    let pkt = build_encrypted_packet(
                        salt,
                        session_id,
                        s_msg_id,
                        seq_counter * 2,
                        &pong,
                        &server_key,
                        Side::Server,
                    );
                    let _ = peer.tx.send(Ok(pkt));
                } else if ctor == 0x1234_5678 {
                    let tag = u32::from_le_bytes(inner[4..8].try_into().unwrap());
                    // Send a gzip-packed RPC result + an unsolicited generated Updates object!
                    let reply_raw = DummyReply { value: tag * 10 }.to_bytes();
                    let gz_reply = Boxed(GzipPacked::pack(&reply_raw).unwrap()).to_bytes();
                    let rpc_res = boxed_rpc_result(m.msg_id, gz_reply);
                    let s_msg_id = ((now_secs as u64) << 32) | ((msg_counter << 2) | 1);
                    msg_counter += 1;
                    let pkt = build_encrypted_packet(
                        salt,
                        session_id,
                        s_msg_id,
                        seq_counter * 2 + 1,
                        &rpc_res,
                        &server_key,
                        Side::Server,
                    );
                    seq_counter += 1;
                    let _ = peer.tx.send(Ok(pkt));

                    // Push a real generated `updatesTooLong#e317af7e`.
                    let update_body = tl_enums::Updates::TooLong.to_bytes();
                    let u_msg_id = ((now_secs as u64) << 32) | ((msg_counter << 2) | 3);
                    msg_counter += 1;
                    let u_pkt = build_encrypted_packet(
                        salt,
                        session_id,
                        u_msg_id,
                        seq_counter * 2 + 1,
                        &update_body,
                        &server_key,
                        Side::Server,
                    );
                    seq_counter += 1;
                    let _ = peer.tx.send(Ok(u_pkt));
                }
            }
        }
    });

    let reply = tokio::time::timeout(Duration::from_secs(5), client.invoke(&DummyCall { tag: 9 }))
        .await
        .expect("invoke timeout")
        .expect("invoke ok");
    assert_eq!(reply, DummyReply { value: 90 });

    let update = tokio::time::timeout(Duration::from_secs(5), updates_rx.recv())
        .await
        .expect("update timeout")
        .expect("update msg");
    assert_eq!(update.dc_id, 2);
    assert_eq!(update.updates(), Some(&tl_enums::Updates::TooLong));
    assert_eq!(update.body, tl_enums::Updates::TooLong.to_bytes());

    client.shutdown();
    server_task.abort();
}

#[tokio::test]
async fn async_client_auto_migrates_on_phone_migrate_and_handles_flood() {
    let factory = Arc::new(FakeTransportFactory::new());
    let mut dc2_peer = factory.push_connection().await;
    let mut dc4_peer = factory.push_connection().await;

    let cfg = ClientConfig {
        api_id: 12345,
        use_pfs: false,
        main_dc_id: 2,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::with_transport(cfg, factory.clone());
    let key2 = test_auth_key(0x22);
    let key4 = test_auth_key(0x44);
    client.import_persistent_keys(vec![(2, key2.clone()), (4, key4.clone())]);

    let now_secs = tg_mtproto::time::local_unix_secs() as i32;

    // DC2 returns PHONE_MIGRATE_4.
    let t2 = tokio::spawn(async move {
        let mut mc = 1u64;
        while let Some(raw) = dc2_peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key2, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else {
                    boxed_rpc_result(m.msg_id, boxed_rpc_error(303, "PHONE_MIGRATE_4"))
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt =
                    build_encrypted_packet(0x2222, session_id, s_id, 0, &resp, &key2, Side::Server);
                let _ = dc2_peer.tx.send(Ok(pkt));
            }
        }
    });

    // DC4 returns FLOOD_WAIT_0 on first call, then succeeds on retry.
    let t4 = tokio::spawn(async move {
        let mut mc = 1u64;
        let mut seen_calls = 0u32;
        while let Some(raw) = dc4_peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key4, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else {
                    seen_calls += 1;
                    if seen_calls == 1 {
                        boxed_rpc_result(m.msg_id, boxed_rpc_error(420, "FLOOD_WAIT_0"))
                    } else {
                        boxed_rpc_result(m.msg_id, DummyReply { value: 444 }.to_bytes())
                    }
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt =
                    build_encrypted_packet(0x4444, session_id, s_id, 0, &resp, &key4, Side::Server);
                let _ = dc4_peer.tx.send(Ok(pkt));
            }
        }
    });

    let res = tokio::time::timeout(Duration::from_secs(5), client.invoke(&DummyCall { tag: 1 }))
        .await
        .expect("timeout")
        .expect("migrated & retried ok");
    assert_eq!(res, DummyReply { value: 444 });
    assert_eq!(client.main_dc_id(), 4);

    client.shutdown();
    t2.abort();
    t4.abort();
}

#[tokio::test]
async fn async_client_guest_dc_auth_export_import_and_reconnect_resend() {
    let factory = Arc::new(FakeTransportFactory::new());
    // Connection order: DC 5 (first attempt), then DC 2 (main DC export), then DC 5 reconnect.
    let mut dc5_peer1 = factory.push_connection().await;
    let mut dc2_peer = factory.push_connection().await;
    let mut dc5_peer2 = factory.push_connection().await;

    let cfg = ClientConfig {
        api_id: 12345,
        use_pfs: false,
        main_dc_id: 2,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::with_transport(cfg, factory.clone());
    let key2 = test_auth_key(0x12);
    let key5 = test_auth_key(0x15);
    client.import_persistent_keys(vec![(2, key2.clone()), (5, key5.clone())]);
    let now_secs = tg_mtproto::time::local_unix_secs() as i32;

    // DC 2 handles auth.exportAuthorization.
    let t2 = tokio::spawn(async move {
        let mut mc = 1u64;
        while let Some(raw) = dc2_peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key2, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else if ctor == tl_functions::auth::ExportAuthorization::CONSTRUCTOR_ID {
                    let exported = tl_enums::auth::ExportedAuthorization::ExportedAuthorization(
                        tl_types::auth::ExportedAuthorization {
                            id: 42,
                            bytes: b"auth-token".to_vec(),
                        },
                    )
                    .to_bytes();
                    boxed_rpc_result(m.msg_id, exported)
                } else {
                    continue;
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt =
                    build_encrypted_packet(0x2200, session_id, s_id, 0, &resp, &key2, Side::Server);
                let _ = dc2_peer.tx.send(Ok(pkt));
            }
        }
    });

    // DC 5 conn 1 returns 401 on DummyCall, accepts auth.importAuthorization, then drops TCP
    // before replying to the retried DummyCall -> DC 5 conn 2 receives the resent call and replies!
    let k5_a = key5.clone();
    let t5_a = tokio::spawn(async move {
        let mut mc = 1u64;
        let mut imported = false;
        while let Some(raw) = dc5_peer1.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &k5_a, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else if ctor == tl_functions::auth::ImportAuthorization::CONSTRUCTOR_ID {
                    imported = true;
                    let auth = tl_enums::auth::Authorization::SignUpRequired(
                        tl_types::auth::AuthorizationSignUpRequired {
                            terms_of_service: None,
                        },
                    )
                    .to_bytes();
                    boxed_rpc_result(m.msg_id, auth)
                } else if ctor == 0x1234_5678 {
                    if !imported {
                        boxed_rpc_result(m.msg_id, boxed_rpc_error(401, "AUTH_KEY_UNREGISTERED"))
                    } else {
                        // Drop TCP to test reconnect + resend_all!
                        let _ = dc5_peer1.tx.send(Err(TransportError::Disconnected));
                        return;
                    }
                } else {
                    continue;
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt =
                    build_encrypted_packet(0x5500, session_id, s_id, 0, &resp, &k5_a, Side::Server);
                let _ = dc5_peer1.tx.send(Ok(pkt));
            }
        }
    });

    let t5_b = tokio::spawn(async move {
        let mut mc = 100u64;
        while let Some(raw) = dc5_peer2.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key5, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else if ctor == 0x1234_5678 {
                    boxed_rpc_result(m.msg_id, DummyReply { value: 555 }.to_bytes())
                } else {
                    continue;
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt =
                    build_encrypted_packet(0x5500, session_id, s_id, 0, &resp, &key5, Side::Server);
                let _ = dc5_peer2.tx.send(Ok(pkt));
            }
        }
    });

    let sender = client.sender();
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        sender.request(&DummyCall { tag: 5 }).to_dc(5).send(),
    )
    .await
    .expect("timeout")
    .expect("auth-imported and resent ok");
    assert_eq!(reply, DummyReply { value: 555 });

    client.shutdown();
    t2.abort();
    t5_a.abort();
    t5_b.abort();
}

// ============================================================================
// 8. Ignored live network tests against Telegram DC2 (149.154.167.50:443)
// ============================================================================

/// Live anonymous MTProto handshake + encrypted ping/pong test against Telegram DC2
/// (`149.154.167.50:443`) using `tg-transport` obfuscated Intermediate framing.
/// Requires no `api_id` and no user account.
#[tokio::test]
#[ignore = "live network test against Telegram DC2 (149.154.167.50:443)"]
async fn live_dc2_create_permanent_auth_key_and_ping_pong() -> Result<(), InvocationError> {
    let cfg = ClientConfig {
        use_pfs: false,
        main_dc_id: 2,
        transport: TransportKind::Intermediate,
        obfuscated: true,
        use_http: false,
        use_ipv6: false,
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::new(cfg);

    // Explicitly target 149.154.167.50:443 for DC 2.
    client
        .dc_options()
        .set_from_list(&[make_dc_option(2, "149.154.167.50", 443, false, true)]);

    let (perm_key, initial_salt) =
        tokio::time::timeout(Duration::from_secs(20), client.create_permanent_auth_key(2))
            .await
            .map_err(|_| InvocationError::Timeout)??;
    assert_ne!(perm_key.key_id(), 0);
    assert_ne!(initial_salt, 0);

    // Send an encrypted ping and verify the server replies with matching pong.
    let pong = tokio::time::timeout(Duration::from_secs(20), client.ping(Some(2)))
        .await
        .map_err(|_| InvocationError::Timeout)??;
    assert_ne!(pong.ping_id, 0);
    assert_ne!(pong.msg_id, 0);

    client.shutdown();
    Ok(())
}

/// Live anonymous MTProto handshake + encrypted ping/pong test against Telegram DC2
/// (`149.154.167.50:443`) using `tg-transport` obfuscated PaddedIntermediate framing (`0xdddddddd`).
#[tokio::test]
#[ignore = "live network test against Telegram DC2 with obfuscated padded intermediate"]
async fn live_dc2_obfuscated_padded_intermediate_auth_key_and_ping_pong()
-> Result<(), InvocationError> {
    let cfg = ClientConfig {
        use_pfs: false,
        main_dc_id: 2,
        transport: TransportKind::PaddedIntermediate,
        obfuscated: true,
        use_http: false,
        use_ipv6: false,
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::new(cfg);

    client
        .dc_options()
        .set_from_list(&[make_dc_option(2, "149.154.167.50", 443, false, true)]);

    let (perm_key, initial_salt) =
        tokio::time::timeout(Duration::from_secs(20), client.create_permanent_auth_key(2))
            .await
            .map_err(|_| InvocationError::Timeout)??;
    assert_ne!(perm_key.key_id(), 0);
    assert_ne!(initial_salt, 0);

    let pong = tokio::time::timeout(Duration::from_secs(20), client.ping(Some(2)))
        .await
        .map_err(|_| InvocationError::Timeout)??;
    assert_ne!(pong.ping_id, 0);
    assert_ne!(pong.msg_id, 0);

    client.shutdown();
    Ok(())
}

/// Live `help.getNearestDc` call wrapped in `invokeWithLayer(LAYER, initConnection(...))`
/// against Telegram DC2 (`149.154.167.50:443`). Runs only when `RUSTGRAM_API_ID` (or legacy
/// `TGFORK_API_ID`) is set in the environment (skips cleanly otherwise; never hardcodes any `api_id`).
#[tokio::test]
#[ignore = "live network test calling help.getNearestDc when RUSTGRAM_API_ID is set"]
async fn live_dc2_help_get_nearest_dc_if_api_id_set() -> Result<(), InvocationError> {
    let Some(api_id) = std::env::var("RUSTGRAM_API_ID")
        .or_else(|_| std::env::var("TGFORK_API_ID"))
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
    else {
        eprintln!("skipping live_dc2_help_get_nearest_dc_if_api_id_set: RUSTGRAM_API_ID not set");
        return Ok(());
    };

    let cfg = ClientConfig {
        api_id,
        use_pfs: false,
        main_dc_id: 2,
        transport: TransportKind::Intermediate,
        obfuscated: true,
        use_http: false,
        use_ipv6: false,
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::new(cfg);
    client
        .dc_options()
        .set_from_list(&[make_dc_option(2, "149.154.167.50", 443, false, true)]);

    let _ = tokio::time::timeout(Duration::from_secs(20), client.create_permanent_auth_key(2))
        .await
        .map_err(|_| InvocationError::Timeout)??;

    let res = tokio::time::timeout(
        Duration::from_secs(20),
        client.invoke(&tl_functions::help::GetNearestDc {}),
    )
    .await
    .map_err(|_| InvocationError::Timeout)?;

    if api_id > 0 {
        let tl_enums::NearestDc::NearestDc(nearest) = res?;
        assert!(!nearest.country.is_empty());
        assert_eq!(nearest.this_dc, 2);
        assert!(nearest.nearest_dc > 0);
    } else {
        // With api_id == 0, Telegram responds with API_ID_INVALID (proving full encrypted RPC roundtrip).
        assert!(matches!(res, Ok(_) | Err(InvocationError::Rpc(_))));
    }

    client.shutdown();
    Ok(())
}

#[test]
fn invoke_after_msg_and_request_cancellation_in_session_state() {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock,
        dcenter,
        0xCAFE,
    );

    // Send request 1 first so it is in have_sent.
    state.enqueue_request(QueuedRequest::new(1, DummyCall { tag: 1 }.to_bytes()));
    let out1 = state.build_next_outbound().expect("req1");
    let req1_msg_id = out1.msg_id;

    // Enqueue request 2 depending on request 1, plus request 3 which we cancel before send.
    let mut req2 = QueuedRequest::new(2, DummyCall { tag: 2 }.to_bytes());
    req2.after_request_id = Some(1);
    state.enqueue_request(req2);
    state.enqueue_request(QueuedRequest::new(3, DummyCall { tag: 3 }.to_bytes()));
    assert!(state.cancel_request(3));

    // Only request 2 is built, and its wire body must start with invokeAfterMsg(req1_msg_id, ...).
    let out2 = state.build_next_outbound().expect("req2");
    let parsed_after =
        tl_functions::InvokeAfterMsg::<RawObject>::from_bytes(&out2.body).expect("invokeAfterMsg");
    assert_eq!(parsed_after.msg_id as u64, req1_msg_id);
    assert_eq!(parsed_after.query.0, DummyCall { tag: 2 }.to_bytes());

    // Cancelling an already-sent request removes it from have_sent.
    assert!(state.cancel_request(1));
    assert_eq!(state.have_sent_len(), 1);
}

#[test]
fn ping_resends_after_bad_server_salt_preserving_ping_id() {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock.clone(),
        dcenter,
        0,
    );

    // Caller queues an explicit ping before transport connects.
    state.schedule_ping(0x1122_3344_5566_7788);
    state.on_transport_connected();
    assert!(!state.is_connected_state());

    // First outbound packet is our explicit ping (not overwritten).
    let out1 = state.build_next_outbound().expect("first ping");
    assert_eq!(out1.salt, 0);
    assert_eq!(
        out1.body,
        mtp_functions::Ping {
            ping_id: 0x1122_3344_5566_7788
        }
        .to_bytes()
    );

    // Server responds with bad_server_salt -> adopts salt, becomes connected, re-queues same ping!
    let bad_salt = make_server_envelope(
        &state,
        0,
        clock.now_secs(),
        1,
        0,
        mtp_enums::BadMsgNotification::BadServerSalt(mtp_types::BadServerSalt {
            bad_msg_id: out1.msg_id as i64,
            bad_msg_seqno: out1.seq_no as i32,
            error_code: 48,
            new_server_salt: 0xABCD_EF01_2345_6789u64 as i64,
        })
        .to_bytes(),
    );
    let outcome = state.handle_decrypted(bad_salt);
    assert_eq!(outcome.action, Some(SessionAction::FlushOutbound));
    assert!(state.is_connected_state());
    assert_eq!(state.salts().active_salt(), 0xABCD_EF01_2345_6789);

    // Second outbound re-sends the exact same ping_id with the new server salt.
    let out2 = state.build_next_outbound().expect("resent ping");
    assert_eq!(out2.salt, 0xABCD_EF01_2345_6789);
    let pong_env = make_server_envelope(
        &state,
        0xABCD_EF01_2345_6789,
        clock.now_secs(),
        2,
        0,
        boxed_pong(out2.msg_id as i64, 0x1122_3344_5566_7788),
    );
    let pong_res = state.handle_decrypted(pong_env);
    assert_eq!(
        pong_res.events,
        vec![SessionEvent::Pong(mtp_types::Pong {
            msg_id: out2.msg_id as i64,
            ping_id: 0x1122_3344_5566_7788,
        })]
    );
}

#[test]
fn dc_options_endpoint_constructor_check() {
    let opts = DcOptions::new(Environment::Test);
    opts.add_endpoint(DcEndpoint {
        id: 2,
        flags: DcOptionFlags(DcOptionFlags::STATIC),
        ip: "149.154.167.41".to_string(),
        port: 443,
        secret: Vec::new(),
    });
    let found = opts.lookup(2, DcType::Regular, true);
    assert!(found.ipv4_tcp.iter().any(|e| e.ip == "149.154.167.41"));
}

// ============================================================================
// 9. DcOptions & Config tdesktop QDataStream persistence tests
// ============================================================================

#[test]
fn dc_options_serialize_roundtrip_with_secrets_cdn_keys_and_temp_exclusion() {
    let opts = DcOptions::new(Environment::Production);
    // Add an endpoint with a 17-byte 0xDD secret and a temporary DC endpoint (which must NOT be serialized).
    let mut secret = vec![0xDDu8];
    secret.extend_from_slice(&[0x42u8; 16]);
    opts.add_endpoint(DcEndpoint {
        id: 2,
        flags: DcOptionFlags(DcOptionFlags::TCPO_ONLY | DcOptionFlags::SECRET),
        ip: "149.154.167.99".to_string(),
        port: 443,
        secret: secret.clone(),
    });
    let temp_dc = temporary_dc_id_from_real(2);
    assert_eq!(temp_dc, 1002);
    opts.add_endpoint(DcEndpoint {
        id: temp_dc,
        flags: DcOptionFlags(DcOptionFlags::TCPO_ONLY),
        ip: "149.154.167.88".to_string(),
        port: 443,
        secret: Vec::new(),
    });

    // Install a CDN public key (reusing a valid RSA public key PEM).
    let cdn_key = rsa::PublicKey::from_pem(BUILT_IN_RSA_KEYS_PRODUCTION[0]).unwrap();
    opts.set_cdn_keys(&[(203, cdn_key.clone())]);

    let bytes = opts.serialize();
    // Version header must be big-endian i32(-2).
    assert_eq!(&bytes[0..4], &(-2i32).to_be_bytes());

    let reloaded = DcOptions::from_serialized(Environment::Production, &bytes).expect("reload");
    let dc2 = reloaded.endpoints_for_dc(2);
    assert!(
        dc2.iter()
            .any(|e| e.ip == "149.154.167.99" && e.secret == secret)
    );
    // Temporary DC 1002 must have been excluded during serialize.
    assert!(reloaded.endpoints_for_dc(temp_dc).is_empty());
    // CDN key round-tripped cleanly with identical fingerprint.
    assert!(reloaded.has_cdn_keys_for_dc(203));
    let loaded_cdn = reloaded
        .get_dc_rsa_key(203, &[cdn_key.fingerprint()])
        .expect("cdn rsa key");
    assert_eq!(loaded_cdn.fingerprint(), cdn_key.fingerprint());

    // Corrupt/truncated data returns false without panicking.
    assert!(!reloaded.construct_from_serialized(&bytes[..6]));
    assert!(!reloaded.construct_from_serialized(&[]));
}

#[test]
fn config_serialize_roundtrip_and_apply_tl_config() {
    let mut cfg = Config::new(Environment::Production);
    let sample_tl = tl_types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: true,
        revoke_pm_inbox: true,
        blocked_mode: true,
        force_try_ipv6: false,
        date: 1_700_000_000,
        expires: 1_700_003_600,
        test_mode: false,
        this_dc: 2,
        dc_options: vec![make_dc_option(2, "149.154.167.50", 443, false, true)],
        dc_txt_domain_name: "apv3.stel.com".to_string(),
        chat_size_max: 250,
        megagroup_size_max: 200_000,
        forwarded_count_max: 100,
        online_update_period_ms: 120_000,
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
        call_receive_timeout_ms: 20_000,
        call_ring_timeout_ms: 90_000,
        call_connect_timeout_ms: 30_000,
        call_packet_timeout_ms: 10_000,
        me_url_prefix: "https://t.me/".to_string(),
        autoupdate_url_prefix: None,
        gif_search_username: Some("gif".to_string()),
        venue_search_username: None,
        img_search_username: None,
        static_maps_provider: None,
        caption_length_max: 2048,
        message_length_max: 4096,
        webfile_dc_id: 4,
        suggested_lang_code: Some("en".to_string()),
        lang_pack_version: Some(12),
        base_lang_pack_version: Some(5),
        reactions_default: Some(tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: "\u{2764}\u{FE0F}".to_string(),
        })),
        autologin_token: Some("tok123".to_string()),
    };

    // Boxed Config wire roundtrip via generated `tg_tl::enums::Config`.
    let boxed_cfg = tl_enums::Config::Config(sample_tl.clone());
    let tl_bytes = boxed_cfg.to_bytes();
    assert_eq!(tl_enums::Config::from_bytes(&tl_bytes).unwrap(), boxed_cfg);

    // Applying a mismatched test_mode config must be rejected.
    let mut wrong_mode = sample_tl.clone();
    wrong_mode.test_mode = true;
    assert!(!cfg.apply(&wrong_mode));

    // Applying a matching config updates ConfigFields + DcOptions.
    assert!(cfg.apply(&sample_tl));
    assert_eq!(cfg.values().chat_size_max, 250);
    assert_eq!(cfg.values().megagroup_size_max, 200_000);
    assert!(cfg.values().blocked_mode);
    assert!(cfg.values().revoke_private_inbox);
    assert_eq!(cfg.values().caption_length_max, 2048);
    assert_eq!(cfg.values().reaction_default_emoji, "\u{2764}\u{FE0F}");
    assert_eq!(cfg.values().autologin_token, "tok123");

    // Binary QDataStream roundtrip matches tdesktop format.
    let bytes = cfg.serialize();
    assert_eq!(&bytes[0..4], &1i32.to_be_bytes()); // version = 1
    assert_eq!(&bytes[4..8], &0i32.to_be_bytes()); // Environment::Production = 0

    let restored = Config::from_serialized(&bytes).expect("restore config");
    assert_eq!(restored.environment(), Environment::Production);
    assert_eq!(restored.values().chat_size_max, 250);
    assert_eq!(restored.values().megagroup_size_max, 200_000);
    assert_eq!(restored.values().caption_length_max, 2048);
    assert_eq!(restored.values().reaction_default_emoji, "\u{2764}\u{FE0F}");
    assert_eq!(restored.values().gif_search_username, "gif");
    assert_eq!(
        restored.dc_options().endpoints_for_dc(2)[0].ip,
        "149.154.167.50"
    );

    // Client-level persistence helper roundtrip.
    let (client, _) = Client::with_transport(
        ClientConfig::default(),
        Arc::new(FakeTransportFactory::new()),
    );
    assert!(client.load_serialized_config(&bytes));
    assert_eq!(client.config_fields().chat_size_max, 250);
    let re_serialized = client.serialize_config();
    assert_eq!(re_serialized, bytes);

    // Sanity check ConfigFields defaults for Test environment.
    let test_fields = ConfigFields::for_environment(Environment::Test);
    assert_eq!(test_fields.txt_domain_string, "tapv3.stel.com");
}

// ============================================================================
// 10. SpecialConfigRequest (DoH TXT + RSA/AES-CBC decrypt + rules) tests
// ============================================================================

#[test]
fn special_config_dns_parsing_phone_rules_http_date_and_payload_decryption() {
    // 1. Random padding bounds (13..=128 alphanumeric).
    for _ in 0..20 {
        let pad = generate_dns_random_padding();
        assert!((13..=128).contains(&pad.len()));
        assert!(pad.bytes().all(|b| b.is_ascii_alphanumeric()));
    }

    // 2. DoH JSON parsing and descending-length TXT concatenation.
    let doh_json = br#"{
        "Status": 0,
        "Answer": [
            {"name": "apv3.stel.com.", "type": 1, "TTL": 60, "data": "1.2.3.4"},
            {"name": "apv3.stel.com.", "type": 16, "TTL": 300, "data": "\"SHORT\""},
            {"name": "apv3.stel.com.", "type": 16, "TTL": 300, "data": "\"MUCHLONGERPART\""}
        ]
    }"#;
    let entries = parse_dns_response(doh_json, Some(DNS_TYPE_TXT));
    assert_eq!(
        entries,
        vec![
            DnsEntry {
                data: "\"SHORT\"".to_string(),
                ttl: 300,
            },
            DnsEntry {
                data: "\"MUCHLONGERPART\"".to_string(),
                ttl: 300,
            },
        ]
    );
    let concat = concatenate_dns_txt_fields(&entries);
    assert_eq!(concat, b"\"MUCHLONGERPART\"\"SHORT\"");

    // 3. Phone prefix rules (`CheckPhoneByPrefixesRules`).
    assert!(check_phone_by_prefixes_rules("", ""));
    assert!(check_phone_by_prefixes_rules("+7 (999) 123-45-67", "+7"));
    assert!(!check_phone_by_prefixes_rules(
        "+7 (999) 123-45-67",
        "+7,-799"
    ));
    assert!(check_phone_by_prefixes_rules("+1 555 0100", ",-7"));
    assert!(!check_phone_by_prefixes_rules("+7 900", ",-7"));

    // 4. HTTP Date parsing (`Wed, 10 Jul 2019 14:33:38 GMT` == 1562769218).
    assert_eq!(
        parse_http_date("Wed, 10 Jul 2019 14:33:38 GMT"),
        Some(1_562_769_218)
    );
    assert_eq!(parse_http_date("invalid date"), None);

    // 5. Known-answer test for post-RSA block decryption (`decrypt_simple_config_rsa_block`).
    let secret_bytes = vec![0xDDu8; 17];
    let simple = mtp_types::help::ConfigSimple {
        date: 1_700_000_000,
        expires: 1_700_086_400,
        rules: vec![
            mtp_enums::AccessPointRule::AccessPointRule(mtp_types::AccessPointRule {
                phone_prefix_rules: String::new(),
                dc_id: 2,
                ips: vec![
                    mtp_enums::IpPort::IpPort(mtp_types::IpPort {
                        ipv4: i32::from_be_bytes([149, 154, 167, 50]),
                        port: 443,
                    }),
                    mtp_enums::IpPort::Secret(mtp_types::IpPortSecret {
                        ipv4: i32::from_be_bytes([95, 161, 76, 100]),
                        port: 8443,
                        secret: secret_bytes.clone(),
                    }),
                ],
            }),
            mtp_enums::AccessPointRule::AccessPointRule(mtp_types::AccessPointRule {
                phone_prefix_rules: "+999".to_string(),
                dc_id: 4,
                ips: vec![mtp_enums::IpPort::IpPort(mtp_types::IpPort {
                    ipv4: i32::from_be_bytes([149, 154, 167, 91]),
                    port: 443,
                })],
            }),
        ],
    };
    let tl_encoded = mtp_enums::help::ConfigSimple::ConfigSimple(simple.clone()).to_bytes();
    let real_len = (4 + tl_encoded.len()) as u32;
    let mut data_208 = vec![0u8; 208];
    data_208[0..4].copy_from_slice(&real_len.to_le_bytes());
    data_208[4..4 + tl_encoded.len()].copy_from_slice(&tl_encoded);

    let digest = sha256(&[&data_208]);
    let mut plaintext_224 = Vec::with_capacity(224);
    plaintext_224.extend_from_slice(&data_208);
    plaintext_224.extend_from_slice(&digest[0..16]);

    let aes_key = [0x37u8; 32];
    let mut aes_iv = [0u8; 16];
    aes_iv.copy_from_slice(&aes_key[16..32]);
    let aes_ct = aes_cbc_encrypt(&plaintext_224, &aes_key, &aes_iv).unwrap();

    let mut rsa_block = [0u8; 256];
    rsa_block[0..32].copy_from_slice(&aes_key);
    rsa_block[32..256].copy_from_slice(&aes_ct);

    let decoded = decrypt_simple_config_rsa_block(&rsa_block).expect("decrypt simple config");
    assert_eq!(decoded, simple);

    // Extracting endpoints filters by phone prefix and expiry.
    let eps = extract_special_endpoints(&decoded, 1_700_000_100, "").expect("extract");
    assert_eq!(
        eps,
        vec![
            SpecialEndpoint {
                dc_id: 2,
                ip: "149.154.167.50".to_string(),
                port: 443,
                secret: Vec::new(),
            },
            SpecialEndpoint {
                dc_id: 2,
                ip: "95.161.76.100".to_string(),
                port: 8443,
                secret: secret_bytes,
            },
        ]
    );
    // Expired config is rejected.
    assert!(matches!(
        extract_special_endpoints(&decoded, 1_700_090_000, ""),
        Err(SpecialConfigError::ExpiredOrEmpty(_))
    ));

    // Tampered ciphertext triggers BadDigest.
    let mut tampered = rsa_block;
    tampered[250] ^= 0x01;
    assert!(matches!(
        decrypt_simple_config_rsa_block(&tampered),
        Err(SpecialConfigError::BadDigest)
    ));

    // Built-in SPECIAL_CONFIG_PUBLIC_KEY parses and rejects malformed Base64 length.
    assert!(matches!(
        decrypt_simple_config(b"short"),
        Err(SpecialConfigError::InvalidBase64(_))
    ));
}

// ============================================================================
// 11. ConfigLoader, KeysDestroyer, http_wait & Connection Racing tests
// ============================================================================

#[tokio::test]
async fn config_loader_loads_help_get_config_and_updates_client() {
    assert_eq!(next_enum_dc(&[1, 2, 3, 4, 5], 2), 3);
    assert_eq!(next_enum_dc(&[1, 2, 3, 4, 5], 5), 1);
    assert_eq!(
        compute_config_refresh_delay(1_700_010_000, 1_700_000_000),
        Duration::from_secs(3600)
    );
    assert_eq!(
        compute_config_refresh_delay(1_700_000_120, 1_700_000_000),
        Duration::from_secs(120)
    );

    let factory = Arc::new(FakeTransportFactory::new());
    let mut peer = factory.push_connection().await;

    let cfg = ClientConfig {
        api_id: 12345,
        use_pfs: false,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::with_transport(cfg, factory.clone());
    let key = test_auth_key(0x61);
    client.import_persistent_keys(vec![(2, key.clone())]);

    let server_key = key.clone();
    let server = tokio::spawn(async move {
        let now_secs = tg_mtproto::time::local_unix_secs() as i32;
        let mut mc = 1u64;
        while let Some(raw) = peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &server_key, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else if ctor == tl_functions::help::GetConfig::CONSTRUCTOR_ID {
                    let tl_cfg = tl_types::Config {
                        default_p2p_contacts: false,
                        preload_featured_stickers: false,
                        revoke_pm_inbox: false,
                        blocked_mode: false,
                        force_try_ipv6: false,
                        date: now_secs,
                        expires: now_secs + 3600,
                        test_mode: false,
                        this_dc: 2,
                        dc_options: vec![make_dc_option(2, "149.154.167.50", 443, false, true)],
                        dc_txt_domain_name: "apv3.stel.com".to_string(),
                        chat_size_max: 333,
                        megagroup_size_max: 200_000,
                        forwarded_count_max: 100,
                        online_update_period_ms: 120_000,
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
                        stickers_recent_limit: 30,
                        channels_read_media_period: 604_800,
                        tmp_sessions: None,
                        call_receive_timeout_ms: 20_000,
                        call_ring_timeout_ms: 90_000,
                        call_connect_timeout_ms: 30_000,
                        call_packet_timeout_ms: 10_000,
                        me_url_prefix: "https://t.me/".to_string(),
                        autoupdate_url_prefix: None,
                        gif_search_username: Some("gif".to_string()),
                        venue_search_username: None,
                        img_search_username: None,
                        static_maps_provider: None,
                        caption_length_max: 1024,
                        message_length_max: 4096,
                        webfile_dc_id: 4,
                        suggested_lang_code: None,
                        lang_pack_version: None,
                        base_lang_pack_version: None,
                        reactions_default: None,
                        autologin_token: None,
                    };
                    boxed_rpc_result(m.msg_id, tl_enums::Config::Config(tl_cfg).to_bytes())
                } else {
                    continue;
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt = build_encrypted_packet(
                    0x7700,
                    session_id,
                    s_id,
                    0,
                    &resp,
                    &server_key,
                    Side::Server,
                );
                let _ = peer.tx.send(Ok(pkt));
            }
        }
    });

    let loader = ConfigLoader::new(
        client.clone(),
        ConfigLoaderOptions {
            enable_doh_fallback: false,
            ..ConfigLoaderOptions::default()
        },
    );
    let loaded = tokio::time::timeout(Duration::from_secs(5), loader.load())
        .await
        .expect("timeout")
        .expect("config load ok");
    assert_eq!(loaded.chat_size_max, 333);
    assert_eq!(client.config_fields().chat_size_max, 333);
    assert!(client.last_config_loaded().is_some());

    client.shutdown();
    server.abort();
}

#[tokio::test]
async fn keys_destroyer_runs_logout_and_destroy_auth_key_and_handles_404() {
    // Verify unique slot assignment for duplicate keys on the same DC.
    let s1 = destroy_key_next_dc_id(2);
    let s2 = destroy_key_next_dc_id(s1);
    assert_ne!(s1, s2);
    assert_eq!(bare_dc_id(s1), 2);
    assert_eq!(bare_dc_id(s2), 2);

    let factory = Arc::new(FakeTransportFactory::new());
    let peer_ok = factory.push_connection().await;
    let peer_404 = factory.push_connection().await;

    let key_ok = test_auth_key(0x91);
    let key_404 = test_auth_key(0x92);
    let dc_options = DcOptions::new(Environment::Production);

    let destroyer = KeysDestroyer::with_transport(
        ClientConfig {
            ping_interval: Duration::from_secs(300),
            ..ClientConfig::default()
        },
        dc_options,
        factory.clone(),
        vec![(2, key_ok.clone()), (4, key_404.clone())],
    )
    .with_timeout(Duration::from_secs(5));
    assert_eq!(destroyer.pending_slots().len(), 2);

    let k_ok_srv = key_ok.clone();
    let k_404_srv = key_404.clone();
    // Spawn a helper that answers whichever peer gets key_ok vs key_404.
    let spawn_destroy_peer =
        |mut peer: tg_mtproto::FakeServerPeer, k_ok: AuthKey, k_404: AuthKey| {
            tokio::spawn(async move {
                let now_secs = tg_mtproto::time::local_unix_secs() as i32;
                let mut mc = 1u64;
                let mut saw_logout = false;
                while let Some(raw) = peer.rx.recv().await {
                    if parse_encrypted_packet(&raw, &k_404, Side::Client).is_ok() {
                        let _ = peer.tx.send(Err(TransportError::ServerError(
                            TransportErrorCode::AuthKeyNotFound,
                        )));
                        return false;
                    }
                    let env = parse_encrypted_packet(&raw, &k_ok, Side::Client).unwrap();
                    let session_id = env.session_id;
                    for m in unpack_or_single(env) {
                        let inner = unwrap_inner_query(&m.body.0);
                        let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                        let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                            let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                            boxed_pong(m.msg_id, ping.ping_id)
                        } else if ctor == tl_functions::auth::LogOut::CONSTRUCTOR_ID {
                            saw_logout = true;
                            let logged_out =
                                tl_enums::auth::LoggedOut::LoggedOut(tl_types::auth::LoggedOut {
                                    future_auth_token: None,
                                })
                                .to_bytes();
                            boxed_rpc_result(m.msg_id, logged_out)
                        } else if ctor == mtp_functions::DestroyAuthKey::CONSTRUCTOR_ID {
                            assert!(saw_logout, "auth.logOut must precede destroy_auth_key");
                            boxed_rpc_result(
                                m.msg_id,
                                mtp_enums::DestroyAuthKeyRes::DestroyAuthKeyOk.to_bytes(),
                            )
                        } else {
                            continue;
                        };
                        let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                        mc += 1;
                        let pkt = build_encrypted_packet(
                            0x8811,
                            session_id,
                            s_id,
                            0,
                            &resp,
                            &k_ok,
                            Side::Server,
                        );
                        let _ = peer.tx.send(Ok(pkt));
                    }
                }
                saw_logout
            })
        };

    let t1 = spawn_destroy_peer(peer_ok, k_ok_srv.clone(), k_404_srv.clone());
    let t2 = spawn_destroy_peer(peer_404, k_ok_srv, k_404_srv);

    let result = destroyer.destroy_all().await;
    assert!(result.all_destroyed());
    assert_eq!(result.outcomes.len(), 2);
    assert!(
        result
            .outcomes
            .iter()
            .any(|&(dc, _, ref o)| dc == 2 && *o == KeyDestroyOutcome::Destroyed)
    );
    assert!(
        result
            .outcomes
            .iter()
            .any(|&(dc, _, ref o)| dc == 4 && *o == KeyDestroyOutcome::AlreadyDestroyed)
    );
    t1.abort();
    t2.abort();
}

#[tokio::test]
async fn http_wait_long_poll_state_machine_and_async_session() {
    // 1. Synchronous SessionState verification: standalone http_wait + bundled with RPC.
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    let mut state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock,
        dcenter,
        0xCAFE,
    );
    state.set_using_http_wait(true);
    assert!(state.is_using_http_wait());

    // Without request_http_wait or queued messages, nothing is emitted.
    assert!(state.build_next_outbound().is_none());

    // Standalone http_wait emitted once when requested.
    state.request_http_wait();
    let standalone = state.build_next_outbound().expect("standalone http_wait");
    assert_eq!(standalone.seq_no % 2, 0, "http_wait must be non-content");
    let mtp_enums::HttpWait::HttpWait(parsed) =
        mtp_enums::HttpWait::from_bytes(&standalone.body).expect("valid HttpWait");
    assert_eq!(parsed.max_delay, HTTP_WAIT_MAX_DELAY_MS);
    assert_eq!(parsed.wait_after, HTTP_WAIT_WAIT_AFTER_MS);
    assert_eq!(parsed.max_wait, HTTP_WAIT_MAX_WAIT_MS);
    // Consumed once -> does not infinite-loop.
    assert!(state.build_next_outbound().is_none());

    // With a user RPC queued, http_wait is bundled at the end of a msg_container.
    state.enqueue_request(QueuedRequest::new(1, DummyCall { tag: 11 }.to_bytes()));
    let bundled = state
        .build_next_outbound()
        .expect("container with http_wait");
    let items = unpack_container(&bundled.body).expect("container");
    assert_eq!(items.len(), 2);
    assert_eq!(
        u32::from_le_bytes(items[1].body.0[0..4].try_into().unwrap()),
        mtp_types::HttpWait::CONSTRUCTOR_ID
    );

    // 2. End-to-end async HTTP session test: empty 200 OK triggers fresh http_wait, then receives update.
    let factory = Arc::new(FakeTransportFactory::new());
    let mut peer = factory.push_http_connection().await;

    let cfg = ClientConfig {
        api_id: 12345,
        use_pfs: false,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, mut updates_rx) = Client::with_transport(cfg, factory.clone());
    let key = test_auth_key(0x73);
    client.import_persistent_keys(vec![(2, key.clone())]);

    let server_key = key.clone();
    let server = tokio::spawn(async move {
        let now_secs = tg_mtproto::time::local_unix_secs() as i32;
        let mut mc = 1u64;
        let mut http_wait_count = 0usize;
        while let Some(raw) = peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &server_key, Side::Client).unwrap();
            let session_id = env.session_id;
            let msgs = unpack_or_single(env);
            let mut replied = false;
            for m in &msgs {
                let ctor = u32::from_le_bytes(m.body.0[0..4].try_into().unwrap());
                if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&m.body.0).unwrap();
                    let pong = boxed_pong(m.msg_id, ping.ping_id);
                    let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                    mc += 1;
                    let pkt = build_encrypted_packet(
                        0x5555,
                        session_id,
                        s_id,
                        0,
                        &pong,
                        &server_key,
                        Side::Server,
                    );
                    let _ = peer.tx.send(Ok(pkt));
                    replied = true;
                } else if ctor == mtp_types::HttpWait::CONSTRUCTOR_ID {
                    http_wait_count += 1;
                    if !replied {
                        if http_wait_count == 1 {
                            // First standalone http_wait returns empty HTTP 200 OK.
                            let _ = peer.tx.send(Ok(Vec::new()));
                        } else {
                            // Second http_wait delivers an unsolicited generated update!
                            let update_body = tl_enums::Updates::TooLong.to_bytes();
                            let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 3);
                            mc += 1;
                            let pkt = build_encrypted_packet(
                                0x5555,
                                session_id,
                                s_id,
                                1,
                                &update_body,
                                &server_key,
                                Side::Server,
                            );
                            let _ = peer.tx.send(Ok(pkt));
                        }
                    }
                }
            }
        }
    });

    // Kick off session via ping, then expect the update delivered over the second http_wait!
    let pong = tokio::time::timeout(Duration::from_secs(5), client.ping(Some(2)))
        .await
        .expect("ping timeout")
        .expect("ping ok");
    assert_ne!(pong.ping_id, 0);

    let update = tokio::time::timeout(Duration::from_secs(5), updates_rx.recv())
        .await
        .expect("http_wait update timeout")
        .expect("update msg");
    assert_eq!(update.dc_id, 2);
    assert_eq!(update.updates(), Some(&tl_enums::Updates::TooLong));
    assert_eq!(update.body, tl_enums::Updates::TooLong.to_bytes());

    client.shutdown();
    server.abort();
}

#[test]
fn race_candidates_builder_prioritizes_transports_ipv6_and_proxies() {
    let opts = DcOptions::new(Environment::Production);
    let cfg = ClientConfig {
        transport: TransportKind::PaddedIntermediate,
        fallback_transports: vec![TransportKind::Intermediate],
        use_ipv4: true,
        use_ipv6: true,
        prefer_ipv6: true,
        use_tcp: true,
        use_http: true,
        ..ClientConfig::default()
    };
    let candidates = build_race_candidates(2, DcType::Regular, &opts, &cfg, &ProxyData::none());
    // DC 2 has 2 IPv4 + 1 IPv6 endpoints:
    // TCP: (2 + 1) * 2 transports = 6, HTTP: 2 + 1 = 3 => 9 candidates total.
    assert_eq!(candidates.len(), 9);
    // Highest priority should be IPv6 TCP with primary transport (ip=2 + tcp=1 + primary=1 = 4).
    let max_prio = candidates.iter().map(|c| c.priority).max().unwrap();
    assert_eq!(max_prio, 4);
    assert!(
        candidates
            .iter()
            .any(|c| matches!(c.endpoint, RaceEndpoint::Http { .. }))
    );

    // MTProxy collapses to a single TCP candidate targeting the proxy server.
    let mtproxy = ProxyData::mtproto("1.2.3.4", 443, "dd00112233445566778899aabbccddeeff");
    let proxy_cands = build_race_candidates(2, DcType::Regular, &opts, &cfg, &mtproxy);
    assert_eq!(proxy_cands.len(), 1);
    assert!(matches!(proxy_cands[0].endpoint, RaceEndpoint::Tcp { .. }));

    // Temporary DC forces IPv4 TCP only (no IPv6, no HTTP).
    let temp_dc = temporary_dc_id_from_real(2);
    opts.add_endpoint(DcEndpoint {
        id: temp_dc,
        flags: DcOptionFlags(DcOptionFlags::TCPO_ONLY),
        ip: "149.154.167.50".to_string(),
        port: 443,
        secret: Vec::new(),
    });
    let temp_cands =
        build_race_candidates(temp_dc, DcType::Temporary, &opts, &cfg, &ProxyData::none());
    assert!(!temp_cands.is_empty());
    assert!(
        temp_cands
            .iter()
            .all(|c| matches!(c.endpoint, RaceEndpoint::Tcp { .. }))
    );
}

// ============================================================================
// 12. Generated tg-tl request/response & RpcClient trait tests
// ============================================================================

async fn invoke_nearest_dc_via_trait<C: RpcClient>(
    rpc: &C,
) -> Result<tl_types::NearestDc, InvocationError> {
    let tl_enums::NearestDc::NearestDc(dc) =
        rpc.invoke(&tl_functions::help::GetNearestDc {}).await?;
    Ok(dc)
}

#[tokio::test]
async fn generated_request_and_response_via_rpc_client_trait_and_parsed_updates() {
    let factory = Arc::new(FakeTransportFactory::new());
    let mut peer = factory.push_connection().await;

    let cfg = ClientConfig {
        api_id: 555_123,
        device_model: "RustgramTestHost".to_string(),
        system_version: "Windows 11".to_string(),
        app_version: "0.2.0".to_string(),
        system_lang_code: "en".to_string(),
        lang_pack: "tdesktop".to_string(),
        lang_code: "en".to_string(),
        tz_offset_secs: Some(3600),
        use_pfs: false,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, mut updates_rx) = Client::with_transport(cfg, factory.clone());
    let mut broadcast_rx = RpcClient::updates(&client);
    let key = test_auth_key(0xA7);
    client.import_persistent_keys(vec![(2, key.clone())]);

    let expected_update = tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
        out: false,
        mentioned: true,
        media_unread: false,
        silent: false,
        id: 101,
        user_id: 777_000,
        message: "hello from generated TL".to_string(),
        pts: 42,
        pts_count: 1,
        date: 1_700_000_500,
        fwd_from: None,
        via_bot_id: None,
        reply_to: None,
        entities: None,
        ttl_period: None,
    });
    let expected_update_server = expected_update.clone();

    let server_key = key.clone();
    let server = tokio::spawn(async move {
        let now_secs = tg_mtproto::time::local_unix_secs() as i32;
        let mut mc = 1u64;
        let mut seq_counter = 0u32;

        while let Some(raw) = peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &server_key, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let raw_ctor = u32::from_le_bytes(m.body.0[0..4].try_into().unwrap());
                if raw_ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&m.body.0).unwrap();
                    let pong = boxed_pong(m.msg_id, ping.ping_id);
                    let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                    mc += 1;
                    let pkt = build_encrypted_packet(
                        0x9988,
                        session_id,
                        s_id,
                        seq_counter * 2,
                        &pong,
                        &server_key,
                        Side::Server,
                    );
                    let _ = peer.tx.send(Ok(pkt));
                    continue;
                }

                // First user request MUST be wrapped in generated InvokeWithLayer<InitConnection<GetNearestDc>>!
                if let Ok(wrapped) = tl_functions::InvokeWithLayer::<
                    tl_functions::InitConnection<tl_functions::help::GetNearestDc>,
                >::from_bytes(&m.body.0)
                {
                    assert_eq!(wrapped.layer, LAYER);
                    assert_eq!(wrapped.query.api_id, 555_123);
                    assert_eq!(wrapped.query.device_model, "RustgramTestHost");
                    assert_eq!(wrapped.query.app_version, "0.2.0");
                    assert!(wrapped.query.params.is_some());

                    let reply = tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                        country: "DE".to_string(),
                        this_dc: 2,
                        nearest_dc: 2,
                    })
                    .to_bytes();
                    let rpc_res = boxed_rpc_result(m.msg_id, reply);
                    let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                    mc += 1;
                    let pkt = build_encrypted_packet(
                        0x9988,
                        session_id,
                        s_id,
                        seq_counter * 2 + 1,
                        &rpc_res,
                        &server_key,
                        Side::Server,
                    );
                    seq_counter += 1;
                    let _ = peer.tx.send(Ok(pkt));

                    // Push a structured Updates::UpdateShortMessage.
                    let upd_bytes = expected_update_server.to_bytes();
                    let u_id = ((now_secs as u64) << 32) | ((mc << 2) | 3);
                    mc += 1;
                    let u_pkt = build_encrypted_packet(
                        0x9988,
                        session_id,
                        u_id,
                        seq_counter * 2 + 1,
                        &upd_bytes,
                        &server_key,
                        Side::Server,
                    );
                    seq_counter += 1;
                    let _ = peer.tx.send(Ok(u_pkt));
                } else if tl_functions::help::GetCdnConfig::from_bytes(&m.body.0).is_ok() {
                    // Second request after initConnection succeeded should NOT be wrapped.
                    let cdn = tl_enums::CdnConfig::CdnConfig(tl_types::CdnConfig {
                        public_keys: vec![tl_enums::CdnPublicKey::CdnPublicKey(
                            tl_types::CdnPublicKey {
                                dc_id: 203,
                                public_key: BUILT_IN_RSA_KEYS_PRODUCTION[0].to_string(),
                            },
                        )],
                    })
                    .to_bytes();
                    let rpc_res = boxed_rpc_result(m.msg_id, cdn);
                    let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                    mc += 1;
                    let pkt = build_encrypted_packet(
                        0x9988,
                        session_id,
                        s_id,
                        seq_counter * 2 + 1,
                        &rpc_res,
                        &server_key,
                        Side::Server,
                    );
                    seq_counter += 1;
                    let _ = peer.tx.send(Ok(pkt));
                }
            }
        }
    });

    assert_eq!(RpcClient::main_dc_id(&client), 2);
    let nearest =
        tokio::time::timeout(Duration::from_secs(5), invoke_nearest_dc_via_trait(&client))
            .await
            .expect("getNearestDc timeout")
            .expect("getNearestDc ok");
    assert_eq!(nearest.country, "DE");
    assert_eq!(nearest.this_dc, 2);
    assert_eq!(nearest.nearest_dc, 2);

    // Both mpsc and RpcClient::updates() broadcast receiver get the parsed Updates + raw bytes.
    let mpsc_upd = tokio::time::timeout(Duration::from_secs(5), updates_rx.recv())
        .await
        .expect("mpsc update timeout")
        .expect("mpsc update");
    assert_eq!(mpsc_upd.dc_id, 2);
    assert_eq!(mpsc_upd.updates(), Some(&expected_update));
    assert_eq!(mpsc_upd.body, expected_update.to_bytes());

    let bcast_upd = tokio::time::timeout(Duration::from_secs(5), broadcast_rx.recv())
        .await
        .expect("broadcast update timeout")
        .expect("broadcast update");
    assert_eq!(bcast_upd, mpsc_upd);

    // Second generated call: help.getCdnConfig -> installs parsed RSA public key for CDN DC 203.
    let loaded_keys = tokio::time::timeout(Duration::from_secs(5), client.request_cdn_config())
        .await
        .expect("getCdnConfig timeout")
        .expect("getCdnConfig ok");
    assert_eq!(loaded_keys, 1);
    assert!(client.dc_options().has_cdn_keys_for_dc(203));

    client.shutdown();
    server.abort();
}

async fn export_and_import_via_trait<C: RpcClient>(
    rpc: &C,
    target_dc: i32,
) -> Result<(i64, Vec<u8>, tl_enums::auth::Authorization), InvocationError> {
    let exported = rpc.export_authorization(target_dc).await?;
    let auth = rpc
        .import_authorization(target_dc, exported.id, exported.bytes.clone())
        .await?;
    Ok((exported.id, exported.bytes, auth))
}

#[tokio::test]
async fn rpc_client_trait_export_import_authorization_and_invoke_in_dc() {
    let factory = Arc::new(FakeTransportFactory::new());
    let mut dc2_peer = factory.push_connection().await;
    let mut dc4_peer = factory.push_connection().await;

    let cfg = ClientConfig {
        api_id: 4242,
        use_pfs: false,
        main_dc_id: 2,
        ping_interval: Duration::from_secs(300),
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::with_transport(cfg, factory.clone());
    let key2 = test_auth_key(0x31);
    let key4 = test_auth_key(0x34);
    client.import_persistent_keys(vec![(2, key2.clone()), (4, key4.clone())]);

    let now_secs = tg_mtproto::time::local_unix_secs() as i32;

    let t2 = tokio::spawn(async move {
        let mut mc = 1u64;
        while let Some(raw) = dc2_peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key2, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else if let Ok(exp_req) =
                    tl_functions::auth::ExportAuthorization::from_bytes(&inner)
                {
                    assert_eq!(exp_req.dc_id, 4);
                    boxed_rpc_result(
                        m.msg_id,
                        tl_enums::auth::ExportedAuthorization::ExportedAuthorization(
                            tl_types::auth::ExportedAuthorization {
                                id: 9001,
                                bytes: b"exported-secret-token".to_vec(),
                            },
                        )
                        .to_bytes(),
                    )
                } else {
                    continue;
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt =
                    build_encrypted_packet(0x2002, session_id, s_id, 0, &resp, &key2, Side::Server);
                let _ = dc2_peer.tx.send(Ok(pkt));
            }
        }
    });

    let t4 = tokio::spawn(async move {
        let mut mc = 1u64;
        while let Some(raw) = dc4_peer.rx.recv().await {
            let env = parse_encrypted_packet(&raw, &key4, Side::Client).unwrap();
            let session_id = env.session_id;
            for m in unpack_or_single(env) {
                let inner = unwrap_inner_query(&m.body.0);
                let ctor = u32::from_le_bytes(inner[0..4].try_into().unwrap());
                let resp = if ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                    let ping = mtp_functions::Ping::from_bytes(&inner).unwrap();
                    boxed_pong(m.msg_id, ping.ping_id)
                } else if let Ok(imp_req) =
                    tl_functions::auth::ImportAuthorization::from_bytes(&inner)
                {
                    assert_eq!(imp_req.id, 9001);
                    assert_eq!(imp_req.bytes, b"exported-secret-token");
                    boxed_rpc_result(
                        m.msg_id,
                        tl_enums::auth::Authorization::SignUpRequired(
                            tl_types::auth::AuthorizationSignUpRequired {
                                terms_of_service: None,
                            },
                        )
                        .to_bytes(),
                    )
                } else if tl_functions::help::GetNearestDc::from_bytes(&inner).is_ok() {
                    boxed_rpc_result(
                        m.msg_id,
                        tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                            country: "NL".to_string(),
                            this_dc: 4,
                            nearest_dc: 4,
                        })
                        .to_bytes(),
                    )
                } else {
                    continue;
                };
                let s_id = ((now_secs as u64) << 32) | ((mc << 2) | 1);
                mc += 1;
                let pkt =
                    build_encrypted_packet(0x4004, session_id, s_id, 0, &resp, &key4, Side::Server);
                let _ = dc4_peer.tx.send(Ok(pkt));
            }
        }
    });

    let (id, bytes, auth) = tokio::time::timeout(
        Duration::from_secs(5),
        export_and_import_via_trait(&client, 4),
    )
    .await
    .expect("export/import timeout")
    .expect("export/import ok");
    assert_eq!(id, 9001);
    assert_eq!(bytes, b"exported-secret-token");
    assert!(matches!(
        auth,
        tl_enums::auth::Authorization::SignUpRequired(_)
    ));

    let tl_enums::NearestDc::NearestDc(dc4_nearest) = tokio::time::timeout(
        Duration::from_secs(5),
        RpcClient::invoke_in_dc(&client, 4, &tl_functions::help::GetNearestDc {}),
    )
    .await
    .expect("invoke_in_dc timeout")
    .expect("invoke_in_dc ok");
    assert_eq!(dc4_nearest.this_dc, 4);
    assert_eq!(dc4_nearest.country, "NL");

    client.shutdown();
    t2.abort();
    t4.abort();
}

#[tokio::test]
async fn client_logout_without_persistent_keys_does_not_dial_or_create_keys() {
    let factory = Arc::new(FakeTransportFactory::new());
    let cfg = ClientConfig {
        api_id: 2040,
        environment: Environment::Test,
        use_pfs: false,
        ..ClientConfig::default()
    };
    let (client, _updates_rx) = Client::with_transport(cfg, factory.clone());

    tokio::time::timeout(Duration::from_secs(2), client.logout())
        .await
        .expect("logout with zero keys should return immediately without dialing");

    assert_eq!(
        factory.connect_calls(),
        0,
        "logout must not open connections or run DH when no persistent keys exist"
    );
    assert!(
        client.export_persistent_keys().is_empty(),
        "logout must not mint any new persistent keys"
    );
    client.shutdown();
}

#[test]
fn shifted_media_session_suppresses_new_session_created_and_push_updates() {
    // Regression for `Session::tryToReceive` (`mtproto/session.cpp:573-580`):
    // shifted upload/download sessions (e.g. `download_dc_id(2, 0)` / `upload_dc_id(0)`)
    // must NOT emit `SessionEvent::Update` for `new_session_created` or pushed updates,
    // which otherwise trigger spurious `updates.getDifference` calls on the main DC.
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    let shifted_download_dc = tg_mtproto::dc_options::download_dc_id(2, 0);
    let mut state = SessionState::new(
        shifted_download_dc,
        DcType::Regular,
        ClientConfig::default(),
        clock.clone(),
        dcenter,
        0x1111,
    );

    state.enqueue_request(QueuedRequest::new(1, DummyCall { tag: 1 }.to_bytes()));
    let out1 = state.build_next_outbound().expect("req1");

    // 1. `new_session_created` still rotates salt and resends, but emits NO `SessionEvent::Update`.
    let nsc_env = make_server_envelope(
        &state,
        0x9999,
        clock.now_secs(),
        1,
        1,
        mtp_enums::NewSession::Created(mtp_types::NewSessionCreated {
            first_msg_id: out1.msg_id as i64,
            unique_id: 777,
            server_salt: 0x9999,
        })
        .to_bytes(),
    );
    let outcome = state.handle_decrypted(nsc_env);
    assert_eq!(state.salts().active_salt(), 0x9999);
    assert!(
        outcome.events.is_empty(),
        "shifted media session must not emit new_session_created as Update event: {:?}",
        outcome.events
    );

    // 2. Arbitrary server-pushed `Updates::TooLong` on shifted session is also suppressed.
    let push_env = make_server_envelope(
        &state,
        0x9999,
        clock.now_secs(),
        2,
        3,
        tl_enums::Updates::TooLong.to_bytes(),
    );
    let push_outcome = state.handle_decrypted(push_env);
    assert!(
        push_outcome.events.is_empty(),
        "shifted media session must not forward pushed Updates: {:?}",
        push_outcome.events
    );
}
