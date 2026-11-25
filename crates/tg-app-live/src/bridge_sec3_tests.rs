//! Security review #3, D4: the UI-drained bridge queues are bounded, critical events survive
//! an overflow, and an overflow requests a state reload.

use super::*;
use tg_app_core::stand_in::types::PeerId;

fn read_event(peer: i64, till: i32) -> IncomingNotifyEvent {
    IncomingNotifyEvent::HistoryRead {
        session_id: 1,
        peer_id: PeerId(peer),
        read_till_id: MsgId(till),
    }
}

fn deleted_event(msg: i32) -> IncomingNotifyEvent {
    IncomingNotifyEvent::MessageDeleted {
        session_id: 1,
        peer_id: PeerId(5),
        msg_id: MsgId(msg),
    }
}

#[test]
fn sec3_d4_session_events_are_bounded_and_keep_force_logout() {
    let state = LiveBridgeState::new();
    state.push_session_event(SessionEvent::ForceLogout {
        notification_type: "AUTH_KEY_DROP_DUPLICATE".to_owned(),
    });
    for i in 0..(MAX_QUEUED_SESSION_EVENTS * 4) {
        state.push_session_event(SessionEvent::UserFullRefreshNeeded {
            user_id: tg_data::UserId(i as u64 + 1),
        });
    }
    let events = state.drain_session_events();
    assert!(
        events.len() <= MAX_QUEUED_SESSION_EVENTS,
        "queue must stay bounded, got {}",
        events.len()
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::ForceLogout { .. })),
        "a forced logout must never be dropped"
    );
    assert!(
        events.iter().any(|e| matches!(e, SessionEvent::Ready)),
        "an overflow queues a state reload request"
    );
    assert!(
        state.history_deltas_lagged.load(Ordering::Acquire),
        "an overflow also reloads the open chat"
    );
}

#[test]
fn sec3_d4_session_events_coalesce_repeats() {
    let state = LiveBridgeState::new();
    for _ in 0..1000 {
        state.push_session_event(SessionEvent::RecentStickers);
        state.push_session_event(SessionEvent::SyncError("timeout".to_owned()));
    }
    state.push_session_event(SessionEvent::SyncError("latest".to_owned()));
    let events = state.drain_session_events();
    assert_eq!(
        events,
        vec![
            SessionEvent::RecentStickers,
            SessionEvent::SyncError("latest".to_owned()),
        ]
    );
}

#[test]
fn sec3_d4_notify_events_are_bounded_and_keep_reads_and_deletes() {
    let state = LiveBridgeState::new();
    for i in 0..50 {
        state.push_notify_event(read_event(9, i));
    }
    state.push_notify_event(deleted_event(77));
    let filler = IncomingNotifyEvent::MessageDeleted {
        session_id: 2,
        peer_id: PeerId(6),
        msg_id: MsgId(1),
    };
    for _ in 0..(MAX_QUEUED_NOTIFY_EVENTS * 3) {
        state.push_notify_event(filler.clone());
    }
    let events = state.drain_notify_events();
    assert_eq!(events.len(), MAX_QUEUED_NOTIFY_EVENTS);
    let reads: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, IncomingNotifyEvent::HistoryRead { .. }))
        .collect();
    assert!(
        reads.len() <= 1,
        "read events for one chat coalesce into the latest one"
    );
}

#[test]
fn sec3_d4_story_updates_are_bounded_and_flag_a_reload() {
    let state = LiveBridgeState::new();
    for i in 0..(MAX_QUEUED_STORY_UPDATES as i32 + 10) {
        state.push_story_update(tl_enums::Update::ReadStories(
            tg_tl::types::UpdateReadStories {
                peer: tl_enums::Peer::User(tg_tl::types::PeerUser {
                    user_id: i64::from(i) + 1,
                }),
                max_id: 5,
            },
        ));
    }
    assert!(state.take_story_updates_overflowed());
    assert!(!state.take_story_updates_overflowed());
    assert_eq!(state.drain_story_updates().len(), MAX_QUEUED_STORY_UPDATES);

    // Read marks for one peer coalesce into the newest one.
    for max_id in 1..=100 {
        state.push_story_update(tl_enums::Update::ReadStories(
            tg_tl::types::UpdateReadStories {
                peer: tl_enums::Peer::User(tg_tl::types::PeerUser { user_id: 7 }),
                max_id,
            },
        ));
    }
    let drained = state.drain_story_updates();
    assert_eq!(drained.len(), 1);
    assert!(!state.take_story_updates_overflowed());
}
