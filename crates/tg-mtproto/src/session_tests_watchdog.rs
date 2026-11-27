//! Sent-request bookkeeping from tdesktop 805bd26 (TGB-3147).

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

fn request(body_len: usize) -> QueuedRequest {
    let mut body = 0x1234_5678u32.to_le_bytes().to_vec();
    body.resize(body_len.max(8), 0);
    let mut req = QueuedRequest::new(next_request_id(), body);
    req.needs_layer = false;
    req
}

/// Sends one request and returns its `msg_id`.
fn send_one(state: &mut SessionState, req: QueuedRequest) -> u64 {
    let request_id = req.request_id;
    state.enqueue_request(req);
    while state.build_next_outbound().is_some() {}
    state.req_to_msg[&request_id]
}

/// A state check used to overwrite the send time, so the request looked freshly sent (and an
/// answer right after the check looked like a fast request for the unixtime correction).
#[test]
fn state_check_keeps_the_send_time() {
    let mut state = connected_state();
    let msg_id = send_one(&mut state, request(8));
    let sent_at = state.have_sent[&msg_id].last_sent_at;
    let checked = Instant::now() + CHECK_SENT_REQUEST_TIMEOUT;
    assert_eq!(
        state.check_sent_requests(checked),
        CheckSentOutcome::QueuedStateRequests
    );
    let req = &state.have_sent[&msg_id];
    assert_eq!(req.last_sent_at, sent_at);
    assert_eq!(req.last_state_check_at, Some(checked));

    // The next check counts from the state check, not from the send.
    while state.build_next_outbound().is_some() {}
    assert_eq!(
        state.check_sent_requests(checked + Duration::from_secs(5)),
        CheckSentOutcome::NothingDue
    );
    assert_eq!(
        state.check_sent_requests(checked + CHECK_SENT_REQUEST_TIMEOUT),
        CheckSentOutcome::QueuedStateRequests
    );
}

/// The check-sent timer is re-armed for the oldest waiting request (805bd26 `nextTimeout`)
/// instead of firing on a fixed 10 s grid.
#[test]
fn next_state_check_follows_the_oldest_waiting_request() {
    let mut state = connected_state();
    assert_eq!(state.next_sent_check_at(), None);
    let msg_id = send_one(&mut state, request(8));
    let sent_at = state.have_sent[&msg_id].last_sent_at.expect("sent");
    assert_eq!(
        state.next_sent_check_at(),
        Some(sent_at + CHECK_SENT_REQUEST_TIMEOUT)
    );
    let checked = sent_at + CHECK_SENT_REQUEST_TIMEOUT;
    state.check_sent_requests(checked);
    assert_eq!(
        state.next_sent_check_at(),
        Some(checked + CHECK_SENT_REQUEST_TIMEOUT)
    );
    state.requests_acked(&[msg_id], true);
    assert_eq!(state.next_sent_check_at(), None);
}

fn body_ctors(out: &OutboundMessage) -> Vec<u32> {
    let ctor = |body: &[u8]| u32::from_le_bytes(body[0..4].try_into().unwrap_or([0; 4]));
    if ctor(&out.body) == MsgContainer::CONSTRUCTOR_ID {
        crate::message::unpack_container(&out.body)
            .unwrap_or_default()
            .iter()
            .map(|m| ctor(&m.body.0))
            .collect()
    } else {
        vec![ctor(&out.body)]
    }
}

/// A request answered between the state check and the next send used to be asked about
/// anyway; `msgs_state_req` now lists only requests that still wait.
#[test]
fn state_request_skips_requests_answered_meanwhile() {
    let mut state = connected_state();
    let msg_id = send_one(&mut state, request(8));
    let checked = Instant::now() + CHECK_SENT_REQUEST_TIMEOUT;
    assert_eq!(
        state.check_sent_requests(checked),
        CheckSentOutcome::QueuedStateRequests
    );
    state.requests_acked(&[msg_id], true);
    assert!(state.have_sent.is_empty());

    let mut ctors = Vec::new();
    while let Some(out) = state.build_next_outbound() {
        ctors.extend(body_ctors(&out));
    }
    assert!(
        !ctors.contains(&mtp_types::MsgsStateReq::CONSTRUCTOR_ID),
        "sent a state request for an answered message: {ctors:x?}"
    );
    assert!(state.state_and_resend_requests.is_empty());
}

/// State and resend requests that were never answered used to stay until the session reset.
#[test]
fn unanswered_state_requests_are_dropped_with_old_containers() {
    let mut state = connected_state();
    send_one(&mut state, request(8));
    let checked = Instant::now() + CHECK_SENT_REQUEST_TIMEOUT;
    state.check_sent_requests(checked);
    while state.build_next_outbound().is_some() {}
    assert_eq!(state.state_and_resend_requests.len(), 1);

    let now = Instant::now();
    state.clear_old_containers(now + Duration::from_secs(10));
    assert_eq!(state.state_and_resend_requests.len(), 1);
    state.clear_old_containers(now + SENT_CONTAINER_LIVES + Duration::from_secs(1));
    assert!(state.state_and_resend_requests.is_empty());
}

/// 805bd26 `haveSentBytes`: unanswered requests count until the server acknowledges them,
/// even while their result is still awaited.
#[test]
fn unanswered_bytes_drop_when_the_server_acknowledges() {
    let mut state = connected_state();
    assert!(state.sends_behind_unanswered());
    let big = send_one(&mut state, request(64 * 1024));
    let small = send_one(&mut state, request(100));
    assert_eq!(state.unanswered_bytes(), (64 * 1024 + 16) + (100 + 16));

    // Acknowledged but the result is still awaited: stays in have_sent, stops counting.
    state.requests_acked(&[big], false);
    assert!(state.have_sent.contains_key(&big));
    assert_eq!(state.unanswered_bytes(), 100 + 16);

    // Sent again (e.g. after a reconnect): counts again until acknowledged.
    state.resend(big);
    while state.build_next_outbound().is_some() {}
    assert_eq!(state.unanswered_bytes(), (64 * 1024 + 16) + (100 + 16));
    let _ = small;
}

/// `http_wait` is held by the server up to `max_wait`, so it must not count as a packet that
/// needs a reply (tdesktop leaves `needAnyResponse` unset for it).
#[test]
fn http_wait_alone_needs_no_response() {
    let mut state = connected_state();
    state.set_using_http_wait(true);
    state.request_http_wait();
    let out = state.build_next_outbound().expect("http_wait packet");
    assert_eq!(body_ctors(&out), [mtp_types::HttpWait::CONSTRUCTOR_ID]);
    assert!(!out.needs_response);
}

#[test]
fn ping_slot_tracks_the_newest_ping() {
    let mut state = connected_state();
    assert!(!state.ping_outstanding());
    state.schedule_ping(7);
    assert!(state.ping_outstanding() && state.ping_is_tracked(7));
    while state.build_next_outbound().is_some() {}
    assert!(state.ping_awaiting_pong());
    state.schedule_ping(9);
    while state.build_next_outbound().is_some() {}
    assert!(!state.ping_is_tracked(7));
    assert!(state.ping_is_tracked(9));
}
