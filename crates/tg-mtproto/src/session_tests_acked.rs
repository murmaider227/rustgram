//! `acked_ids` stays bounded on every insert path (TGB-3075).

use super::*;
use crate::message::next_request_id;

fn connected_state() -> SessionState {
    let dcenter = Dcenter::new(2, None);
    dcenter.set_connection_inited(true);
    SessionState::new(
        2,
        DcType::Regular,
        ClientConfig::default(),
        MtprotoClock::new(),
        dcenter,
        0x2222,
    )
}

fn no_ack_request() -> QueuedRequest {
    let mut body = 0x1234_5678u32.to_le_bytes().to_vec();
    body.extend_from_slice(&[0u8; 4]);
    let mut req = QueuedRequest::new(next_request_id(), body);
    req.needs_layer = false;
    req.need_ack = false;
    req
}

/// Requests sent without an ack used to be added to `acked_ids` with no trim, so the map grew
/// past `IDS_BUFFER_SIZE` until an ack arrived.
#[test]
fn requests_sent_without_ack_keep_acked_ids_bounded() {
    let mut state = connected_state();
    let total = IDS_BUFFER_SIZE * 3;
    let mut sent = 0usize;
    let mut packets = 0usize;
    while sent < total {
        // Several at a time, so both the single-message and the container paths run.
        for _ in 0..(1 + packets % 4).min(total - sent) {
            state.enqueue_request(no_ack_request());
            sent += 1;
        }
        while state.build_next_outbound().is_some() {
            packets += 1;
        }
        assert!(
            state.acked_ids.len() <= IDS_BUFFER_SIZE,
            "acked_ids grew to {} after {sent} no-ack requests",
            state.acked_ids.len()
        );
    }
    assert_eq!(state.acked_ids.len(), IDS_BUFFER_SIZE);
    assert!(state.to_send.is_empty());
}

#[test]
fn remember_acked_keeps_the_newest_ids() {
    let mut state = connected_state();
    let request_id = next_request_id();
    // Out of order, like acks for a container arriving after newer single messages.
    for i in (0..IDS_BUFFER_SIZE as u64 + 50).rev() {
        state.remember_acked(1_000 + i * 4, request_id);
    }
    state.remember_acked(1_000 + IDS_BUFFER_SIZE as u64 * 4 + 400, request_id);
    assert_eq!(state.acked_ids.len(), IDS_BUFFER_SIZE);
    let oldest = state.acked_ids.keys().next().copied();
    assert_eq!(oldest, Some(1_000 + 51 * 4));
    // Re-recording an id already kept does not evict anything.
    state.remember_acked(1_000 + 60 * 4, request_id);
    assert_eq!(state.acked_ids.len(), IDS_BUFFER_SIZE);
    assert_eq!(state.acked_ids.keys().next().copied(), oldest);
}
