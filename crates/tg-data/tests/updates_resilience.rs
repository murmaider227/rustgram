use std::collections::BTreeMap;

use tg_data::*;

fn env(pts: i32, pts_count: i32, payload: &'static str) -> CommonUpdateEnvelope<&'static str> {
    CommonUpdateEnvelope {
        pts,
        pts_count,
        date: None,
        qts: None,
        payload,
    }
}

fn ch_env(
    channel_id: u64,
    pts: i32,
    pts_count: i32,
    payload: &'static str,
) -> ChannelUpdateEnvelope<&'static str> {
    ChannelUpdateEnvelope {
        channel_id: ChannelId(channel_id),
        pts,
        pts_count,
        payload,
    }
}

#[test]
fn pts_duplicate_while_gap_open_does_not_falsely_close_gap() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 1, 1_000);

    // Gap: pts=12 arrives while pts=11 is still missing.
    assert_eq!(
        sm.on_common_update(env(12, 1, "u12"), 1_100),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.pts(), 10);

    // Duplicate pts=12 arrives before pts=11 -> must be skipped, NOT falsely close the gap!
    assert_eq!(
        sm.on_common_update(env(12, 1, "u12_dup"), 1_200),
        vec![UpdateAction::Skip]
    );
    assert_eq!(sm.pts(), 10);
    assert_eq!(
        sm.next_deadline(),
        Some(1_100 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
    );

    // Now the real missing pts=11 arrives -> closes gap cleanly with [u11, u12] (no duplicate!).
    assert_eq!(
        sm.on_common_update(env(11, 1, "u11"), 1_300),
        vec![UpdateAction::Apply("u11"), UpdateAction::Apply("u12")]
    );
    assert_eq!(sm.pts(), 12);
    assert_eq!(sm.next_deadline(), None);
}

#[test]
fn channel_pts_duplicate_while_gap_open_is_skipped() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 1, 1_000);
    sm.init_channel_pts(ChannelId(42), 50);

    assert_eq!(
        sm.on_channel_update(ch_env(42, 52, 1, "c52"), 1_100),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(
        sm.on_channel_update(ch_env(42, 52, 1, "c52_dup"), 1_200),
        vec![UpdateAction::Skip]
    );
    assert_eq!(sm.channel_pts(ChannelId(42)), Some(50));

    assert_eq!(
        sm.on_channel_update(ch_env(42, 51, 1, "c51"), 1_300),
        vec![
            UpdateAction::ApplyChannel {
                channel_id: ChannelId(42),
                payload: "c51",
            },
            UpdateAction::ApplyChannel {
                channel_id: ChannelId(42),
                payload: "c52",
            },
        ]
    );
    assert_eq!(sm.channel_pts(ChannelId(42)), Some(52));
}

#[test]
fn seq_duplicate_buffered_entry_is_skipped() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 5, 1_000);

    assert_eq!(
        sm.on_seq_updates(7, 7, 1_020, "seq7_first", 1_100),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(
        sm.on_seq_updates(7, 7, 1_020, "seq7_dup", 1_150),
        vec![UpdateAction::Skip]
    );
    assert_eq!(
        sm.on_seq_updates(6, 6, 1_015, "seq6", 1_200),
        vec![
            UpdateAction::Apply("seq6"),
            UpdateAction::Apply("seq7_first"),
        ]
    );
    assert_eq!(sm.seq(), 7);
}

#[test]
fn qts_ordering_gap_fill_timeout_and_duplicates() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 5, 1, 1_000);
    assert_eq!(sm.qts(), 5);

    // Duplicate qts <= 5 skipped.
    assert_eq!(
        sm.on_qts_update(5, "q5_dup", 1_050),
        vec![UpdateAction::Skip]
    );

    // Gap: qts=7 buffered with 1s timeout; duplicate qts=7 skipped.
    assert_eq!(sm.on_qts_update(7, "q7", 1_100), vec![UpdateAction::Buffer]);
    assert_eq!(
        sm.on_qts_update(7, "q7_dup", 1_150),
        vec![UpdateAction::Skip]
    );
    assert_eq!(
        sm.next_deadline(),
        Some(1_100 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
    );

    // Missing qts=6 arrives in time -> drains [q6, q7] in order.
    assert_eq!(
        sm.on_qts_update(6, "q6", 1_300),
        vec![UpdateAction::Apply("q6"), UpdateAction::Apply("q7")]
    );
    assert_eq!(sm.qts(), 7);
    assert_eq!(sm.next_deadline(), None);

    // Unfilled qts gap triggers getDifference after WAIT_FOR_SKIPPED_TIMEOUT_MS.
    assert_eq!(sm.on_qts_update(9, "q9", 2_000), vec![UpdateAction::Buffer]);
    assert_eq!(
        sm.on_tick(2_000 + WAIT_FOR_SKIPPED_TIMEOUT_MS),
        vec![UpdateAction::RequestDifference {
            pts: 10,
            date: 1_000,
            qts: 7,
        }]
    );
}

#[test]
fn min_channel_timer_blocks_date_advance_and_fires_get_difference() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 2, 1, 5_000);

    // Unknown/min channel update arms _byMinChannelTimer (1s) and prevents date advance.
    sm.on_missing_channel(5_100);
    assert_eq!(
        sm.next_deadline(),
        Some(5_100 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
    );
    sm.advance_date(1_500);
    assert_eq!(sm.date(), 1_000);

    // At 5_100 + 1_000ms, fires getDifference with the held date (1_000).
    assert_eq!(
        sm.on_tick(5_100 + WAIT_FOR_SKIPPED_TIMEOUT_MS),
        vec![UpdateAction::RequestDifference {
            pts: 10,
            date: 1_000,
            qts: 2,
        }]
    );
}

#[test]
fn difference_too_long_resets_pts_and_clears_buffers() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 3, 5, 1_000);
    assert_eq!(
        sm.on_updates_too_long(1_500),
        vec![UpdateAction::RequestDifference {
            pts: 10,
            date: 1_000,
            qts: 3,
        }]
    );

    assert_eq!(
        sm.on_difference_too_long(250, 1_600),
        vec![UpdateAction::DifferenceTooLongReset { pts: 250 }]
    );
    assert_eq!(sm.pts(), 250);
    assert!(!sm.requesting_difference());
    assert_eq!(sm.next_deadline(), None);
}

#[test]
fn check_last_update_idle_and_after_sleep_resets_timestamp() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 1, 10_000);

    assert!(!sm.check_last_update(false, 10_000 + NO_UPDATES_TIMEOUT_MS));
    assert!(sm.check_last_update(false, 10_000 + NO_UPDATES_TIMEOUT_MS + 1));
    // Immediately after returning true, last_update_ms was updated so it does not repeat.
    assert!(!sm.check_last_update(false, 10_000 + NO_UPDATES_TIMEOUT_MS + 500));

    // After sleep threshold passes again, triggers once more.
    assert!(sm.check_last_update(
        true,
        10_000 + NO_UPDATES_TIMEOUT_MS + 1 + NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS + 1
    ));
}

#[test]
fn on_reconnect_resets_seq_and_refreshes_active_channels() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(25, 2_000, 4, 12, 1_000);
    sm.init_channel_pts(ChannelId(100), 80);
    sm.init_channel_pts(ChannelId(200), 90);
    sm.set_active_channel(ChannelId(100), true, 1_100);

    let actions = sm.on_reconnect(2_000);
    assert_eq!(sm.seq(), 0);
    assert!(actions.contains(&UpdateAction::RequestDifference {
        pts: 25,
        date: 2_000,
        qts: 4,
    }));
    // Only active channel 100 is refreshed; inactive channel 200 is not polled.
    // `api/api_updates.cpp:743-748`: `from = Unknown` with `!waitingForSkipped` sends `force = false`.
    assert!(actions.contains(&UpdateAction::RequestChannelDifference {
        channel_id: ChannelId(100),
        pts: 80,
        force: false,
        limit: CHANNEL_GET_DIFF_LIMIT,
    }));
    assert!(!actions.iter().any(|a| matches!(
        a,
        UpdateAction::RequestChannelDifference { channel_id, .. } if *channel_id == ChannelId(200)
    )));
}

#[test]
fn persisted_updates_state_binary_roundtrip_and_corruption_rejection() {
    let mut channels = BTreeMap::new();
    channels.insert(ChannelId(11), 300);
    channels.insert(ChannelId(22), 450);
    let original = PersistedUpdatesState {
        pts: 1234,
        qts: 56,
        date: 1_750_000_000,
        seq: 89,
        channel_pts: channels,
    };
    assert!(original.is_valid());

    let encoded = original.to_bytes();
    let decoded = PersistedUpdatesState::from_bytes(&encoded).expect("valid decode");
    assert_eq!(decoded, original);

    // Corrupt a payload byte -> CRC mismatch rejected cleanly without panic.
    let mut corrupted = encoded.clone();
    corrupted[10] ^= 0xFF;
    assert_eq!(PersistedUpdatesState::from_bytes(&corrupted), None);

    // Truncated bytes rejected cleanly.
    assert_eq!(PersistedUpdatesState::from_bytes(&encoded[..12]), None);

    // Restore into a fresh UpdatesStateMachine and re-export.
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.restore_persisted_state(&decoded, 5_000);
    assert!(!sm.requesting_difference());
    assert_eq!(sm.pts(), 1234);
    assert_eq!(sm.qts(), 56);
    assert_eq!(sm.date(), 1_750_000_000);
    assert_eq!(sm.seq(), 89);
    assert_eq!(sm.channel_pts(ChannelId(11)), Some(300));
    assert_eq!(sm.channel_pts(ChannelId(22)), Some(450));
    assert_eq!(sm.export_persisted_state(), original);
}

#[test]
fn peer_loaded_rejects_minimal_and_unloaded_peers() {
    let mut session = Session::new(UserId(1));

    let mut min_user = User::new(UserId(10), false);
    min_user.common.loaded_status = LoadedStatus::Minimal;
    session.upsert_user(min_user, PeerUpdateFlags::NAME);

    assert!(session.peer_minimal_loaded(PeerId::from_user(UserId(10))));
    assert!(!session.peer_loaded(PeerId::from_user(UserId(10))));
    assert!(!session.user_loaded(UserId(10)));

    let mut full_user = User::new(UserId(10), false);
    full_user.common.loaded_status = LoadedStatus::Normal;
    session.upsert_user(full_user, PeerUpdateFlags::NAME);

    assert!(session.peer_loaded(PeerId::from_user(UserId(10))));
    assert!(session.user_loaded(UserId(10)));

    let mut min_ch = Channel::new(ChannelId(55));
    min_ch.common.loaded_status = LoadedStatus::Minimal;
    session.upsert_channel(min_ch, PeerUpdateFlags::NAME);
    assert!(!session.channel_loaded(ChannelId(55)));
}
