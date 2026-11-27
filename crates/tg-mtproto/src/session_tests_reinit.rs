//! Re-initialization generation from tdesktop 3f11c74 / bff150f (TGB-3153).

use super::*;
use crate::message::next_request_id;

fn state_with(dcenter: &Dcenter) -> SessionState {
    SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        MtprotoClock::new(),
        dcenter.clone(),
        0x2222,
    )
}

fn send_request(state: &mut SessionState) -> u64 {
    let mut body = 0x1234_5678u32.to_le_bytes().to_vec();
    body.extend_from_slice(&[0u8; 4]);
    let req = QueuedRequest::new(next_request_id(), body);
    let request_id = req.request_id;
    state.enqueue_request(req);
    while state.build_next_outbound().is_some() {}
    state.req_to_msg[&request_id]
}

fn rpc_result_for(state: &SessionState, counter: u64, req_msg_id: u64) -> DecryptedEnvelope {
    let now = MtprotoClock::new().now_secs() as u32 as u64;
    let mut reply = 0x8765_4321u32.to_le_bytes().to_vec();
    reply.extend_from_slice(&7u32.to_le_bytes());
    DecryptedEnvelope {
        server_salt: 0x2222,
        session_id: state.session_id(),
        msg_id: (now << 32) | (counter << 2) | 1,
        seq_no: (counter as u32) * 2 + 1,
        body: Boxed(RpcResult {
            req_msg_id: req_msg_id as i64,
            result: RawObject(reply),
        })
        .to_bytes(),
    }
}

/// 3f11c74: the language changed (a re-initialization started) while a request sent on the
/// old connection was still in flight. Its late answer used to mark the DC inited, so the
/// next request went out without `initConnection` and the server kept the old language.
#[test]
fn late_answer_after_a_reinit_does_not_mark_the_dc_inited() {
    let dcenter = Dcenter::new(2, None);
    let mut state = state_with(&dcenter);
    state.on_transport_connected();
    let old = send_request(&mut state);

    dcenter.begin_reinit();
    state.handle_decrypted(rpc_result_for(&state, 1, old));
    assert!(
        !dcenter.connection_inited(),
        "a late answer marked the re-initialized DC inited"
    );

    // After the restart the connection belongs to the new generation: its answer counts.
    state.on_transport_connected();
    let fresh = send_request(&mut state);
    state.handle_decrypted(rpc_result_for(&state, 2, fresh));
    assert!(dcenter.connection_inited());
}

/// bff150f: another session sharing the DC (e.g. a temporary config session) sees the same
/// generation, so its late answer cannot mark the DC inited either.
#[test]
fn reinit_generation_is_shared_by_sessions_of_the_dc() {
    let dcenter = Dcenter::new(2, None);
    let mut main = state_with(&dcenter);
    let mut other = state_with(&dcenter);
    main.on_transport_connected();
    other.on_transport_connected();
    let pending = send_request(&mut other);

    dcenter.begin_reinit();
    main.on_transport_connected();
    other.handle_decrypted(rpc_result_for(&other, 1, pending));
    assert!(!dcenter.connection_inited());
}
