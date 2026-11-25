//! Regression tests for SEC-C2: one shared nesting-depth, message-count, and allocation
//! budget across `msg_container` and `gzip_packed` receive handling.

use tg_mtproto::auth_key::Dcenter;
use tg_mtproto::dc_options::DcType;
use tg_mtproto::message::{
    DecryptedEnvelope, MAX_RECEIVE_MESSAGES, MAX_RECEIVE_NESTING, ReceiveBudget, pack_container,
    unpack_container, unpack_container_with_budget, unwrap_gzip_layers_with_budget,
};
use tg_mtproto::session::{SessionAction, SessionState};
use tg_mtproto::tl::mtproto::{CoreMessage, GzipPacked, enums as mtp_enums, types as mtp_types};
use tg_mtproto::tl::{Boxed, RawObject, Serializable};
use tg_mtproto::{ClientConfig, MtprotoClock};

fn make_session() -> (SessionState, MtprotoClock) {
    let clock = MtprotoClock::new();
    let dcenter = Dcenter::new(2, None);
    let state = SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        clock.clone(),
        dcenter,
        0x1111,
    );
    (state, clock)
}

fn server_msg_id(clock: &MtprotoClock, seq: u64) -> u64 {
    let secs = clock.now_secs() as u64;
    // Server reply msg_id: `(secs << 32) | ((seq << 2) | 1)`.
    (secs << 32) | (((seq & 0x1FFF_FFFF) << 2) | 1)
}

fn empty_msgs_ack_bytes() -> Vec<u8> {
    mtp_enums::MsgsAck::MsgsAck(mtp_types::MsgsAck {
        msg_ids: Vec::new(),
    })
    .to_bytes()
}

fn make_envelope(
    state: &SessionState,
    clock: &MtprotoClock,
    seq: u64,
    body: Vec<u8>,
) -> DecryptedEnvelope {
    DecryptedEnvelope {
        server_salt: 0x1111,
        session_id: state.session_id(),
        msg_id: server_msg_id(clock, seq),
        seq_no: 0,
        body,
    }
}

#[test]
fn nested_msg_container_bomb_is_rejected_without_stack_overflow() {
    let (mut state, clock) = make_session();

    // Wrap an empty msgs_ack inside MAX_RECEIVE_NESTING + 4 nested msg_containers.
    let mut body = empty_msgs_ack_bytes();
    for i in 0..(MAX_RECEIVE_NESTING + 4) {
        let msg = CoreMessage {
            msg_id: server_msg_id(&clock, (i as u64) + 10) as i64,
            seqno: 0,
            body: RawObject(body),
        };
        body = pack_container(vec![msg]);
    }

    let env = make_envelope(&state, &clock, 1, body);
    let outcome = state.handle_decrypted(env);
    assert_eq!(
        outcome.action,
        Some(SessionAction::RestartConnection),
        "excessive msg_container nesting must be rejected"
    );
}

#[test]
fn interleaved_gzip_and_msg_container_share_nesting_depth() {
    let (mut state, clock) = make_session();

    // Interleave gzip_packed and msg_container: 34 pairs = 68 layers > MAX_RECEIVE_NESTING (64),
    // even though gzip layers alone (34) and container layers alone (34) are each < 64.
    let pairs = (MAX_RECEIVE_NESTING / 2) + 2;
    let mut body = empty_msgs_ack_bytes();
    for i in 0..pairs {
        body = Boxed(GzipPacked::pack(&body).expect("gzip pack")).to_bytes();
        let msg = CoreMessage {
            msg_id: server_msg_id(&clock, (i as u64) + 100) as i64,
            seqno: 0,
            body: RawObject(body),
        };
        body = pack_container(vec![msg]);
    }

    let env = make_envelope(&state, &clock, 2, body);
    let outcome = state.handle_decrypted(env);
    assert_eq!(
        outcome.action,
        Some(SessionAction::RestartConnection),
        "interleaved gzip + msg_container exceeding shared depth must be rejected"
    );
}

#[test]
fn msg_container_message_count_bomb_is_rejected() {
    let (mut state, clock) = make_session();
    let ack = empty_msgs_ack_bytes();

    // 1. Single container exceeding MAX_RECEIVE_MESSAGES is rejected by unpack_container.
    let too_many: Vec<CoreMessage> = (0..=MAX_RECEIVE_MESSAGES)
        .map(|i| CoreMessage {
            msg_id: server_msg_id(&clock, (i as u64) + 10) as i64,
            seqno: 0,
            body: RawObject(ack.clone()),
        })
        .collect();
    let huge_container = pack_container(too_many);
    assert!(
        unpack_container(&huge_container).is_err(),
        "container exceeding MAX_RECEIVE_MESSAGES must fail unpack_container"
    );

    // 2. Two-level container bomb: 100 inner containers x 50 messages = 5,000 > MAX_RECEIVE_MESSAGES (4,096).
    let mut outer_msgs = Vec::with_capacity(100);
    let mut seq = 10u64;
    for _ in 0..100 {
        let mut inner_msgs = Vec::with_capacity(50);
        for _ in 0..50 {
            inner_msgs.push(CoreMessage {
                msg_id: server_msg_id(&clock, seq) as i64,
                seqno: 0,
                body: RawObject(ack.clone()),
            });
            seq += 1;
        }
        let inner_container = pack_container(inner_msgs);
        outer_msgs.push(CoreMessage {
            msg_id: server_msg_id(&clock, seq) as i64,
            seqno: 0,
            body: RawObject(inner_container),
        });
        seq += 1;
    }
    let bomb = pack_container(outer_msgs);
    let env = make_envelope(&state, &clock, 3, bomb);
    let outcome = state.handle_decrypted(env);
    assert_eq!(
        outcome.action,
        Some(SessionAction::RestartConnection),
        "two-level container message-count bomb must be rejected"
    );
}

#[test]
fn shared_allocation_budget_rejects_gzip_inside_container_expansion_bomb() {
    let (mut state, clock) = make_session();

    // One 2.5 MiB zero-padded payload compresses to ~2.5 KiB with gzip.
    // Sixteen copies inside a msg_container decompress to 40 MiB > MAX_RECEIVE_ALLOC_BYTES (32 MiB)
    // while the wire container itself is under 50 KiB.
    let big_unpacked = vec![0u8; 2_500_000];
    let gz_item = Boxed(GzipPacked::pack(&big_unpacked).expect("gzip pack")).to_bytes();
    let msgs: Vec<CoreMessage> = (0..16)
        .map(|i| CoreMessage {
            msg_id: server_msg_id(&clock, (i as u64) + 20) as i64,
            seqno: 0,
            body: RawObject(gz_item.clone()),
        })
        .collect();
    let bomb = pack_container(msgs);
    assert!(bomb.len() < 100_000);

    let env = make_envelope(&state, &clock, 4, bomb);
    let outcome = state.handle_decrypted(env);
    assert_eq!(
        outcome.action,
        Some(SessionAction::RestartConnection),
        "cumulative gzip expansion inside msg_container exceeding shared byte budget must be rejected"
    );

    // Direct ReceiveBudget check: container unpacking + gzip unpacking share the same byte budget.
    let payload = vec![0x42u8; 4096];
    let packed = Boxed(GzipPacked::pack(&payload).expect("gzip pack")).to_bytes();
    let container = pack_container(vec![CoreMessage {
        msg_id: 1,
        seqno: 0,
        body: RawObject(packed),
    }]);
    let mut tight = ReceiveBudget::new(8, 16, 2048);
    let unpacked_msgs =
        unpack_container_with_budget(&container, &mut tight).expect("small container fits budget");
    assert!(
        unwrap_gzip_layers_with_budget(&unpacked_msgs[0].body.0, &mut tight).is_err(),
        "gzip unpack exceeding remaining shared budget must fail"
    );
}

#[test]
fn valid_gzip_wrapped_msg_container_succeeds() {
    let (mut state, clock) = make_session();
    let ack = empty_msgs_ack_bytes();
    let container = pack_container(vec![CoreMessage {
        msg_id: server_msg_id(&clock, 50) as i64,
        seqno: 0,
        body: RawObject(ack),
    }]);
    let gz = Boxed(GzipPacked::pack(&container).expect("gzip pack")).to_bytes();
    let env = make_envelope(&state, &clock, 5, gz);
    let outcome = state.handle_decrypted(env);
    assert_ne!(outcome.action, Some(SessionAction::RestartConnection));
}
