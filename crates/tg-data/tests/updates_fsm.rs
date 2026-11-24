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
fn common_pts_in_order_and_duplicates() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    // Before init_state, requesting_difference() is true (waiting on updates.getState).
    assert!(sm.requesting_difference());
    assert_eq!(
        sm.on_common_update(env(11, 1, "early"), 100),
        vec![UpdateAction::Skip]
    );

    sm.init_state(10, 1_000, 0, 1, 100);
    assert!(!sm.requesting_difference());
    assert_eq!(sm.pts(), 10);

    // Exact next update applies immediately.
    assert_eq!(
        sm.on_common_update(env(11, 1, "u11"), 200),
        vec![UpdateAction::Apply("u11")]
    );
    assert_eq!(sm.pts(), 11);

    // Multi-count contiguous update (11 + 3 == 14) applies immediately.
    assert_eq!(
        sm.on_common_update(env(14, 3, "u14"), 300),
        vec![UpdateAction::Apply("u14")]
    );
    assert_eq!(sm.pts(), 14);

    // Duplicate / stale updates (pts <= good && count > 0) are skipped.
    assert_eq!(
        sm.on_common_update(env(14, 1, "dup14"), 400),
        vec![UpdateAction::Skip]
    );
    assert_eq!(
        sm.on_common_update(env(12, 1, "old12"), 500),
        vec![UpdateAction::Skip]
    );
    assert_eq!(sm.pts(), 14);
    assert_eq!(sm.next_deadline(), None);
}

#[test]
fn common_pts_out_of_order_gap_filled_within_one_second() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 1, 1_000);

    // Receive pts=13 (gap: missing 11, 12) -> buffered, 1s deadline armed.
    assert_eq!(
        sm.on_common_update(env(13, 1, "u13"), 1_100),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.pts(), 10);
    assert_eq!(
        sm.next_deadline(),
        Some(1_100 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
    );

    // Receive pts=12 -> still missing 11, keeps earlier deadline 2_100.
    assert_eq!(
        sm.on_common_update(env(12, 1, "u12"), 1_300),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.next_deadline(), Some(2_100));

    // Receive pts=11 before deadline -> fills gap, drains in strict PTS order [u11, u12, u13]!
    assert_eq!(
        sm.on_common_update(env(11, 1, "u11"), 1_500),
        vec![
            UpdateAction::Apply("u11"),
            UpdateAction::Apply("u12"),
            UpdateAction::Apply("u13"),
        ]
    );
    assert_eq!(sm.pts(), 13);
    assert_eq!(sm.next_deadline(), None);
}

#[test]
fn common_pts_unfilled_gap_triggers_get_difference_after_timeout() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_500, 7, 2, 1_000);

    assert_eq!(
        sm.on_common_update(env(12, 1, "u12"), 2_000),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.next_deadline(), Some(3_000));

    // Tick before deadline: nothing happens.
    assert!(sm.on_tick(2_999).is_empty());
    assert!(!sm.requesting_difference());

    // Tick at deadline: triggers getDifference with local (pts=10, date=1500, qts=7).
    assert_eq!(
        sm.on_tick(3_000),
        vec![UpdateAction::RequestDifference {
            pts: 10,
            date: 1_500,
            qts: 7,
        }]
    );
    assert!(sm.requesting_difference());
    assert_eq!(sm.next_deadline(), None);

    // Live updates while requesting getDifference are skipped.
    assert_eq!(
        sm.on_common_update(env(13, 1, "live13"), 3_100),
        vec![UpdateAction::Skip]
    );

    // Receive a differenceSlice -> applies slice and immediately requests next slice!
    assert_eq!(
        sm.on_difference_slice(12, 1_600, 8, 3, vec!["s11", "s12"], 3_200),
        vec![
            UpdateAction::Apply("s11"),
            UpdateAction::Apply("s12"),
            UpdateAction::RequestDifference {
                pts: 12,
                date: 1_600,
                qts: 8,
            },
        ]
    );
    assert!(sm.requesting_difference());
    assert_eq!(sm.pts(), 12);

    // Receive final difference -> applies and clears requesting.
    assert_eq!(
        sm.on_difference_final(14, 1_700, 9, 4, vec!["f13", "f14"], 3_400),
        vec![UpdateAction::Apply("f13"), UpdateAction::Apply("f14")]
    );
    assert!(!sm.requesting_difference());
    assert_eq!(sm.pts(), 14);
    assert_eq!(sm.date(), 1_700);
    assert_eq!(sm.qts(), 9);
    assert_eq!(sm.seq(), 4);
}

#[test]
fn common_pts_overlap_last_less_than_count_triggers_fast_1ms_timeout() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 1, 1_000);

    // First update: pts=12, count=1 -> last=12, count=11 (last > count -> 1000ms).
    assert_eq!(
        sm.on_common_update(env(12, 1, "a"), 1_000),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.next_deadline(), Some(2_000));

    // Overlapping update: pts=12, count=2 -> last=12, count=13 (last < count -> 1ms!).
    assert_eq!(
        sm.on_common_update(env(12, 2, "b"), 1_050),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.next_deadline(), Some(1_050 + FAST_GAP_TIMEOUT_MS));

    // Fires at 1_051ms. Note: tdesktop starts qts at -1 and ignores qts == 0 in setState.
    assert_eq!(
        sm.on_tick(1_051),
        vec![UpdateAction::RequestDifference {
            pts: 10,
            date: 1_000,
            qts: -1,
        }]
    );
}

#[test]
fn pts_count_zero_applies_immediately_and_arms_gap_timer_if_ahead() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 1, 1_000);

    // pts == good, pts_count == 0 -> applies immediately, no timer.
    assert_eq!(
        sm.on_common_update(env(10, 0, "zero_same"), 1_100),
        vec![UpdateAction::Apply("zero_same")]
    );
    assert_eq!(sm.next_deadline(), None);

    // pts > good, pts_count == 0 -> applies immediately AND schedules 1s gap check.
    assert_eq!(
        sm.on_common_update(env(12, 0, "zero_ahead"), 1_200),
        vec![UpdateAction::Apply("zero_ahead")]
    );
    assert_eq!(sm.next_deadline(), Some(2_200));
}

#[test]
fn seq_gap_buffering_cascading_and_timeout() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 5, 1_000);

    // Stale seq <= 5 skipped.
    assert_eq!(
        sm.on_seq_updates(5, 5, 1_010, "stale", 1_100),
        vec![UpdateAction::Skip]
    );

    // Future seq=7 buffered with 1s deadline.
    assert_eq!(
        sm.on_seq_updates(7, 7, 1_020, "seq7", 1_200),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.next_deadline(), Some(2_200));

    // Combined seq 8..=10 also buffered.
    assert_eq!(
        sm.on_seq_updates(8, 10, 1_030, "seq8_10", 1_300),
        vec![UpdateAction::Buffer]
    );

    // Missing seq=6 arrives -> applies 6, cascades 7, cascades 8..=10!
    assert_eq!(
        sm.on_seq_updates(6, 6, 1_015, "seq6", 1_400),
        vec![
            UpdateAction::Apply("seq6"),
            UpdateAction::Apply("seq7"),
            UpdateAction::Apply("seq8_10"),
        ]
    );
    assert_eq!(sm.seq(), 10);
    assert_eq!(sm.date(), 1_030);
    assert_eq!(sm.next_deadline(), None);

    // Unfilled seq gap times out after 1s and triggers getDifference.
    assert_eq!(
        sm.on_seq_updates(15, 15, 1_040, "seq15", 2_000),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(
        sm.on_tick(3_000),
        vec![UpdateAction::RequestDifference {
            pts: 10,
            date: 1_030,
            qts: -1,
        }]
    );
}

#[test]
fn difference_failure_exponential_backoff() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(10, 1_000, 0, 1, 0);

    // Trigger getDifference via updatesTooLong.
    assert_eq!(
        sm.on_updates_too_long(1_000),
        vec![UpdateAction::RequestDifference {
            pts: 10,
            date: 1_000,
            qts: -1,
        }]
    );

    // Expected backoff sequence in ms: 1s, 2s, 4s, 8s, 16s, 32s, 64s, 64s.
    let mut now = 1_000_u64;
    for expected_secs in [1_u64, 2, 4, 8, 16, 32, 64, 64] {
        sm.on_difference_fail(now);
        let deadline = now + expected_secs * 1_000;
        assert_eq!(sm.next_deadline(), Some(deadline));
        now = deadline;
        assert_eq!(
            sm.on_tick(now),
            vec![UpdateAction::RequestDifference {
                pts: 10,
                date: 1_000,
                qts: -1,
            }]
        );
    }

    // Success resets backoff back to 1s.
    sm.on_difference_empty(1_100, 2, now);
    sm.on_updates_too_long(now + 100);
    sm.on_difference_fail(now + 100);
    assert_eq!(sm.next_deadline(), Some(now + 100 + 1_000));
}

#[test]
fn new_session_resets_seq_and_channel_backoff_and_ping_check() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(20, 5_000, 3, 9, 1_000);
    assert!(!sm.needs_ping(1_000 + NO_UPDATES_TIMEOUT_MS));
    assert!(sm.needs_ping(1_000 + NO_UPDATES_TIMEOUT_MS + 1));

    // new_session_created resets seq to 0 and triggers getDifference.
    assert_eq!(
        sm.on_new_session_created(),
        vec![UpdateAction::RequestDifference {
            pts: 20,
            date: 5_000,
            qts: 3,
        }]
    );
    assert_eq!(sm.seq(), 0);
    sm.on_difference_empty(5_000, 9, 2_000);

    // Independent per-channel failure backoff.
    // `api/api_updates.cpp:743-748`: `updateChannelTooLong` (`from = Unknown`) with
    // `!waitingForSkipped` sends `force = false`.
    sm.init_channel_pts(ChannelId(77), 10);
    assert_eq!(
        sm.on_channel_too_long(ChannelId(77), None, 2_000),
        vec![UpdateAction::RequestChannelDifference {
            channel_id: ChannelId(77),
            pts: 10,
            force: false,
            limit: CHANNEL_GET_DIFF_LIMIT,
        }]
    );
    sm.on_channel_difference_fail(ChannelId(77), 2_000);
    assert_eq!(sm.next_deadline(), Some(3_000));
    assert_eq!(
        sm.on_tick(3_000),
        vec![UpdateAction::RequestChannelDifference {
            channel_id: ChannelId(77),
            pts: 10,
            force: false,
            limit: CHANNEL_GET_DIFF_LIMIT,
        }]
    );
    sm.on_channel_difference_fail(ChannelId(77), 3_000);
    assert_eq!(sm.next_deadline(), Some(5_000)); // doubled to 2s
}

#[test]
fn per_channel_pts_gap_difference_too_long_and_short_poll() {
    let mut sm = UpdatesStateMachine::<&'static str>::new();
    sm.init_state(100, 2_000, 0, 1, 1_000);
    sm.init_channel_pts(ChannelId(555), 50);

    // Gap on channel 555: pts=52 arrives before 51.
    assert_eq!(
        sm.on_channel_update(ch_env(555, 52, 1, "c52"), 1_100),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(sm.next_deadline(), Some(2_100));

    // Gap fills in time.
    assert_eq!(
        sm.on_channel_update(ch_env(555, 51, 1, "c51"), 1_500),
        vec![
            UpdateAction::ApplyChannel {
                channel_id: ChannelId(555),
                payload: "c51",
            },
            UpdateAction::ApplyChannel {
                channel_id: ChannelId(555),
                payload: "c52",
            },
        ]
    );
    assert_eq!(sm.channel_pts(ChannelId(555)), Some(52));
    assert_eq!(sm.next_deadline(), None);

    // Unfilled gap on channel 555 -> RequestChannelDifference { force: true }.
    assert_eq!(
        sm.on_channel_update(ch_env(555, 55, 1, "c55"), 2_000),
        vec![UpdateAction::Buffer]
    );
    assert_eq!(
        sm.on_tick(3_000),
        vec![UpdateAction::RequestChannelDifference {
            channel_id: ChannelId(555),
            pts: 52,
            force: true,
            limit: CHANNEL_GET_DIFF_LIMIT,
        }]
    );

    // Non-final channelDifference chains another RequestChannelDifference immediately
    // with `force: false` (`api/api_updates.cpp:443,743-748`: `from = Unknown`, `!waitingForSkipped`).
    assert_eq!(
        sm.on_channel_difference(ChannelId(555), 54, false, None, vec!["d53", "d54"], 3_100),
        vec![
            UpdateAction::ApplyChannel {
                channel_id: ChannelId(555),
                payload: "d53",
            },
            UpdateAction::ApplyChannel {
                channel_id: ChannelId(555),
                payload: "d54",
            },
            UpdateAction::RequestChannelDifference {
                channel_id: ChannelId(555),
                pts: 54,
                force: false,
                limit: CHANNEL_GET_DIFF_LIMIT,
            },
        ]
    );

    // Mark channel active -> final channelDifference schedules short poll (timeout=5s).
    sm.set_active_channel(ChannelId(555), true, 3_150);
    assert_eq!(
        sm.on_channel_difference_empty(ChannelId(555), 55, true, Some(5), 3_200),
        vec![]
    );
    assert_eq!(sm.channel_pts(ChannelId(555)), Some(55));

    // Short poll timer fires (`from = PtsGapOrShortPoll`) with `force: true` (`api/api_updates.cpp:652,743-748`).
    let poll_time = sm.next_deadline().unwrap();
    assert_eq!(
        sm.on_tick(poll_time),
        vec![UpdateAction::RequestChannelDifference {
            channel_id: ChannelId(555),
            pts: 55,
            force: true,
            limit: CHANNEL_GET_DIFF_LIMIT,
        }]
    );

    // channelDifferenceTooLong emits ChannelTooLongReset and updates pts.
    assert_eq!(
        sm.on_channel_difference_too_long(
            ChannelId(555),
            Some(100),
            true,
            Some(30),
            poll_time + 100
        ),
        vec![UpdateAction::ChannelTooLongReset {
            channel_id: ChannelId(555),
        }]
    );
    assert_eq!(sm.channel_pts(ChannelId(555)), Some(100));

    // updateChannelTooLong with pts <= 100 is ignored; pts > 100 triggers fetch (`force: false`).
    assert!(
        sm.on_channel_too_long(ChannelId(555), Some(99), poll_time + 200)
            .is_empty()
    );
    assert_eq!(
        sm.on_channel_too_long(ChannelId(555), Some(105), poll_time + 300),
        vec![UpdateAction::RequestChannelDifference {
            channel_id: ChannelId(555),
            pts: 100,
            force: false,
            limit: CHANNEL_GET_DIFF_LIMIT,
        }]
    );
}
