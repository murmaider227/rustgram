//! Scripted end-to-end resilience tests for `UpdatesHandler` + `tg_data::Session`
//! using `FakeRpcClient`.
//!
//! Covers:
//! - gap filling itself within `WAIT_FOR_SKIPPED_TIMEOUT_MS`
//! - unfilled gap -> `updates.getDifference`
//! - multi-slice `updates.differenceSlice` chain (including `updateChannelTooLong` inside diff)
//! - `updates.differenceTooLong`
//! - `updates.channelDifferenceTooLong` resetting channel history without merging across gap
//! - `updateShortMessage` / `updateShortChatMessage` with unknown or `min` users -> `getDifference`
//! - `updateShortSentMessage` promotion with server date/entities/ttl
//! - `min` channel in `updateNewChannelMessage` (`_byMinChannelTimer`)
//! - restart resume from `PersistedUpdatesState` (and fallback on `PTS_EXPIRED`)
//! - duplicate updates (common PTS, buffered-gap PTS, channel PTS, seq, qts)

mod common;

use common::{
    FakeRpcClient, rpc_err, sample_tl_channel, sample_tl_dialog, sample_tl_message, sample_tl_user,
};
use tg_api::convert::{apply_tl_chat, apply_tl_user};
use tg_api::updates::{
    NO_UPDATES_TIMEOUT_MS, PersistedUpdatesState, UpdateEvent, UpdatesHandler,
    WAIT_FOR_SKIPPED_TIMEOUT_MS,
};
use tg_data::{
    ChannelId, FullMsgId, LoadedStatus, Message, MessageFlags, MsgId, MsgRange, PeerId,
    SERVER_MAX_MSG_ID, Session, TextWithEntities, UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

fn user_peer(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser { user_id: id })
}

fn channel_peer(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: id })
}

fn tl_state(pts: i32, qts: i32, date: i32, seq: i32) -> tl_types::updates::State {
    tl_types::updates::State {
        pts,
        qts,
        date,
        seq,
        unread_count: 0,
    }
}

fn new_msg_update(
    peer_uid: i64,
    msg_id: i32,
    date: i32,
    text: &str,
    pts: i32,
    pts_count: i32,
) -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: sample_tl_message(
                user_peer(peer_uid),
                Some(user_peer(peer_uid)),
                msg_id,
                date,
                text,
            ),
            pts,
            pts_count,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date,
        seq: 0,
    })
}

#[tokio::test]
async fn gap_fills_itself_within_wait_window_in_strict_order() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    apply_tl_user(
        &mut session,
        &tl_enums::User::User(sample_tl_user(1, 10, "Alpha", false)),
    );
    let u1 = PeerId::from_user(UserId(1));

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        0,
        1_700_000_000,
        1,
    )));

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");
    assert_eq!(handler.pts(), 10);

    // Track exact order of MessageUpdated(NEW_ADDED) change events emitted to tg-data.
    let mut changes_rx = session.subscribe();

    // Push pts=13 then pts=12 (missing 11) -> both buffered, no getDifference yet.
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 103, 1_700_000_013, "third", 13, 1),
            None,
            1_100,
        )
        .await
        .expect("pts 13");
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 102, 1_700_000_012, "second", 12, 1),
            None,
            1_400,
        )
        .await
        .expect("pts 12");

    assert_eq!(handler.pts(), 10);
    assert!(session.message(FullMsgId::new(u1, MsgId(102))).is_none());
    assert!(session.message(FullMsgId::new(u1, MsgId(103))).is_none());
    assert_eq!(
        handler.next_deadline(),
        Some(1_100 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
    );

    // Missing pts=11 arrives before deadline (at 1_800 < 2_100) -> fills gap, zero getDifference!
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 101, 1_700_000_011, "first", 11, 1),
            None,
            1_800,
        )
        .await
        .expect("pts 11");

    assert_eq!(handler.pts(), 13);
    assert_eq!(handler.next_deadline(), None);
    assert!(
        client
            .calls_of::<tl_fns::updates::GetDifference>()
            .is_empty()
    );

    // Verify all three messages were applied in strict PTS order [101, 102, 103].
    let mut applied_ids = Vec::new();
    while let Ok(ev) = changes_rx.try_recv() {
        if let tg_data::ChangeEvent::MessageUpdated { id, .. } = ev
            && id.peer == u1
        {
            applied_ids.push(id.msg);
        }
    }
    assert_eq!(applied_ids, vec![MsgId(101), MsgId(102), MsgId(103)]);
    assert_eq!(
        session.history(u1).and_then(|h| h.top_message_id),
        Some(MsgId(103))
    );
}

#[tokio::test]
async fn gap_unfilled_triggers_get_difference_after_wait_window() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    apply_tl_user(
        &mut session,
        &tl_enums::User::User(sample_tl_user(1, 10, "Alpha", false)),
    );
    let u1 = PeerId::from_user(UserId(1));

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        2,
        1_700_000_000,
        4,
    )));
    client.enqueue_ok::<tl_fns::updates::GetDifference>(
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: vec![
                sample_tl_message(user_peer(1), Some(user_peer(1)), 101, 1_700_000_010, "m101"),
                sample_tl_message(user_peer(1), Some(user_peer(1)), 102, 1_700_000_011, "m102"),
            ],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_state(12, 2, 1_700_000_011, 5)),
        }),
    );

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");

    // Receive short message with pts=12 while pts=11 is missing.
    // Because it is buffered on a PTS gap, local date stays at 1_700_000_000.
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                id: 102,
                user_id: 1,
                message: "m102".to_owned(),
                pts: 12,
                pts_count: 1,
                date: 1_700_000_011,
                fwd_from: None,
                via_bot_id: None,
                reply_to: None,
                entities: None,
                ttl_period: None,
            }),
            None,
            2_000,
        )
        .await
        .expect("buffer");

    // Tick 1ms before deadline: must NOT call getDifference yet.
    handler
        .on_tick(&client, &mut session, 2_999)
        .await
        .expect("tick early");
    assert!(
        client
            .calls_of::<tl_fns::updates::GetDifference>()
            .is_empty()
    );

    // Tick at exact deadline: invokes getDifference with local (pts=10, date=1_700_000_000, qts=2).
    handler
        .on_tick(&client, &mut session, 3_000)
        .await
        .expect("tick due");
    let diffs = client.calls_of::<tl_fns::updates::GetDifference>();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].1.pts, 10);
    assert_eq!(diffs[0].1.date, 1_700_000_000);
    assert_eq!(diffs[0].1.qts, 2);
    assert_eq!(handler.pts(), 12);
    assert!(session.message(FullMsgId::new(u1, MsgId(101))).is_some());
    assert!(session.message(FullMsgId::new(u1, MsgId(102))).is_some());
}

#[tokio::test]
async fn difference_slice_chain_applies_all_slices_and_handles_channel_too_long_inside_diff() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    let u1 = PeerId::from_user(UserId(1));
    let ch_id = ChannelId(77);
    let ch_pid = PeerId::from_channel(ch_id);
    apply_tl_chat(
        &mut session,
        &tl_enums::Chat::Channel(sample_tl_channel(77, 7700, "News", false)),
    );

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        0,
        1_700_000_000,
        1,
    )));
    // Slice 1: pts 10 -> 11
    client.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Slice(
        tl_types::updates::DifferenceSlice {
            new_messages: vec![sample_tl_message(
                user_peer(1),
                Some(user_peer(1)),
                201,
                1_700_000_010,
                "slice-1",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(sample_tl_user(1, 10, "Alpha", false))],
            intermediate_state: tl_enums::updates::State::State(tl_state(11, 1, 1_700_000_010, 2)),
        },
    ));
    // Slice 2: pts 11 -> 12, carries UpdateChannelTooLong(ch=77, pts=35) in other_updates!
    client.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Slice(
        tl_types::updates::DifferenceSlice {
            new_messages: vec![sample_tl_message(
                user_peer(1),
                Some(user_peer(1)),
                202,
                1_700_000_020,
                "slice-2",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: vec![tl_enums::Update::ChannelTooLong(
                tl_types::UpdateChannelTooLong {
                    channel_id: 77,
                    pts: Some(35),
                },
            )],
            chats: Vec::new(),
            users: Vec::new(),
            intermediate_state: tl_enums::updates::State::State(tl_state(12, 2, 1_700_000_020, 3)),
        },
    ));
    // Final diff: pts 12 -> 13
    client.enqueue_ok::<tl_fns::updates::GetDifference>(
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: vec![sample_tl_message(
                user_peer(1),
                Some(user_peer(1)),
                203,
                1_700_000_030,
                "slice-3-final",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_state(13, 3, 1_700_000_030, 4)),
        }),
    );
    // Response for UpdateChannelTooLong emitted inside Slice 2!
    client.enqueue_ok::<tl_fns::updates::GetChannelDifference>(
        &tl_enums::updates::ChannelDifference::ChannelDifference(
            tl_types::updates::ChannelDifference {
                r#final: true,
                pts: 35,
                timeout: Some(15),
                new_messages: vec![sample_tl_message(
                    channel_peer(77),
                    None,
                    300,
                    1_700_000_025,
                    "channel-diff-msg",
                )],
                other_updates: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");
    handler.init_channel_pts(ch_id, 30);

    // Trigger updatesTooLong -> executes 3-step Slice -> Slice -> Final chain + ChannelDifference.
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::TooLong,
            None,
            2_000,
        )
        .await
        .expect("too long");

    let diff_calls = client.calls_of::<tl_fns::updates::GetDifference>();
    assert_eq!(diff_calls.len(), 3);
    assert_eq!(diff_calls[0].1.pts, 10);
    assert_eq!(diff_calls[1].1.pts, 11);
    assert_eq!(diff_calls[1].1.qts, 1);
    assert_eq!(diff_calls[2].1.pts, 12);
    assert_eq!(diff_calls[2].1.qts, 2);

    assert_eq!(handler.pts(), 13);
    assert_eq!(handler.state_snapshot(), (1_700_000_030, 3, 4));
    assert!(session.message(FullMsgId::new(u1, MsgId(201))).is_some());
    assert!(session.message(FullMsgId::new(u1, MsgId(202))).is_some());
    assert!(session.message(FullMsgId::new(u1, MsgId(203))).is_some());

    // ChannelTooLong inside Slice 2 was NOT dropped: it fetched channel difference!
    assert_eq!(
        client
            .calls_of::<tl_fns::updates::GetChannelDifference>()
            .len(),
        1
    );
    assert_eq!(handler.channel_pts(ch_id), Some(35));
    assert!(
        session
            .message(FullMsgId::new(ch_pid, MsgId(300)))
            .is_some()
    );
}

#[tokio::test]
async fn difference_too_long_resets_pts_and_emits_event() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    apply_tl_user(
        &mut session,
        &tl_enums::User::User(sample_tl_user(1, 10, "Alpha", false)),
    );

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        0,
        1_700_000_000,
        1,
    )));
    client.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::TooLong(
        tl_types::updates::DifferenceTooLong { pts: 500 },
    ));

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");

    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::TooLong,
            None,
            2_000,
        )
        .await
        .expect("diff too long");

    assert_eq!(handler.pts(), 500);
    assert!(
        handler
            .drain_events()
            .contains(&UpdateEvent::DifferenceTooLong { pts: 500 })
    );

    // Subsequent update contiguous with new pts=500 applies cleanly.
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 501, 1_700_000_100, "after-reset", 501, 1),
            None,
            2_500,
        )
        .await
        .expect("pts 501");
    assert_eq!(handler.pts(), 501);
    assert!(
        session
            .message(FullMsgId::new(PeerId::from_user(UserId(1)), MsgId(501)))
            .is_some()
    );
}

#[tokio::test]
async fn channel_too_long_resets_history_bottom_without_merging_across_gap() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    let ch_id = ChannelId(88);
    let ch_pid = PeerId::from_channel(ch_id);
    apply_tl_chat(
        &mut session,
        &tl_enums::Chat::Channel(sample_tl_channel(88, 8800, "Updates", false)),
    );

    // Pre-load bottom slice [1, 2, 3] with `loaded_at_bottom == true`.
    let initial_msgs: Vec<Message> = [1, 2, 3]
        .into_iter()
        .map(|id| {
            Message::new_text(
                ch_pid,
                MsgId(id),
                None,
                1_700_000_000 + id as i32,
                TextWithEntities::plain(format!("old {id}")),
            )
        })
        .collect();
    session.add_history_slice(
        ch_pid,
        initial_msgs,
        MsgRange::new(MsgId(1), SERVER_MAX_MSG_ID),
        Some(10),
    );
    assert!(
        session
            .history(ch_pid)
            .expect("history")
            .loaded_ranges
            .loaded_at_bottom()
    );

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        0,
        1_700_000_000,
        1,
    )));
    let mut dialog = sample_tl_dialog(channel_peer(88), 50);
    if let tl_enums::Dialog::Dialog(ref mut d) = dialog {
        d.pts = Some(50);
        d.unread_count = 12;
    }
    client.enqueue_ok::<tl_fns::updates::GetChannelDifference>(
        &tl_enums::updates::ChannelDifference::TooLong(
            tl_types::updates::ChannelDifferenceTooLong {
                r#final: true,
                timeout: Some(20),
                dialog,
                messages: vec![sample_tl_message(
                    channel_peer(88),
                    None,
                    50,
                    1_700_000_500,
                    "latest post 50",
                )],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");
    handler.init_channel_pts(ch_id, 10);

    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ChannelTooLong(tl_types::UpdateChannelTooLong {
                    channel_id: 88,
                    pts: Some(50),
                }),
                date: 1_700_000_500,
            }),
            None,
            2_000,
        )
        .await
        .expect("channel too long");

    assert_eq!(handler.channel_pts(ch_id), Some(50));
    let hist = session.history(ch_pid).expect("history");
    // Bottom MUST be marked not loaded so UI reloads recent history.
    assert!(!hist.loaded_ranges.loaded_at_bottom());
    // Old slice [1..=3] and new snapshot [50] MUST remain separate slices (gap 4..49 preserved!).
    assert_eq!(hist.loaded_ranges.slices().len(), 2);
    assert_eq!(
        hist.loaded_ranges.slices()[0].range,
        MsgRange::new(MsgId(1), MsgId(3))
    );
    assert_eq!(hist.top_message_id, Some(MsgId(50)));
    assert_eq!(hist.unread_count, Some(12));
    assert!(
        handler
            .drain_events()
            .contains(&UpdateEvent::ChannelTooLong {
                channel_id: ch_id,
                pts: Some(50),
            })
    );
}

#[tokio::test]
async fn short_message_with_unknown_and_min_user_triggers_get_difference() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        0,
        1_700_000_000,
        1,
    )));

    // First getDifference: for completely unknown user 55.
    client.enqueue_ok::<tl_fns::updates::GetDifference>(
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: vec![sample_tl_message(
                user_peer(55),
                Some(user_peer(55)),
                301,
                1_700_000_010,
                "hello from unknown",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(sample_tl_user(
                55, 5500, "NewPeer", false,
            ))],
            state: tl_enums::updates::State::State(tl_state(11, 0, 1_700_000_010, 2)),
        }),
    );

    // Second getDifference: for `min: true` user 66 -> upgrades to non-min user 66!
    client.enqueue_ok::<tl_fns::updates::GetDifference>(
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: vec![sample_tl_message(
                user_peer(66),
                Some(user_peer(66)),
                302,
                1_700_000_020,
                "hello from upgraded min user",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(sample_tl_user(
                66, 6600, "FullPeer", false,
            ))],
            state: tl_enums::updates::State::State(tl_state(12, 0, 1_700_000_020, 3)),
        }),
    );

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");

    // 1. Unknown user 55 in UpdateShortMessage -> must trigger getDifference!
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                id: 301,
                user_id: 55,
                message: "hello from unknown".into(),
                pts: 11,
                pts_count: 1,
                date: 1_700_000_010,
                fwd_from: None,
                via_bot_id: None,
                reply_to: None,
                entities: None,
                ttl_period: None,
            }),
            None,
            1_500,
        )
        .await
        .expect("short unknown");

    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 1);
    assert!(session.user_loaded(UserId(55)));
    assert_eq!(
        session.peer_access_hash(PeerId::from_user(UserId(55))),
        Some(5500)
    );
    assert!(
        session
            .message(FullMsgId::new(PeerId::from_user(UserId(55)), MsgId(301)))
            .is_some()
    );

    // 2. Seed user 66 as a `min: true` constructor -> LoadedStatus::Minimal!
    let mut min_user = sample_tl_user(66, 0, "MinOnly", false);
    min_user.min = true;
    min_user.access_hash = None;
    apply_tl_user(&mut session, &tl_enums::User::User(min_user));
    assert_eq!(
        session
            .user(UserId(66))
            .expect("min user")
            .common
            .loaded_status,
        LoadedStatus::Minimal
    );
    assert!(!session.user_loaded(UserId(66)));

    // UpdateShortMessage from `min` user 66 -> MUST trigger getDifference!
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                id: 302,
                user_id: 66,
                message: "hello from upgraded min user".into(),
                pts: 12,
                pts_count: 1,
                date: 1_700_000_020,
                fwd_from: None,
                via_bot_id: None,
                reply_to: None,
                entities: None,
                ttl_period: None,
            }),
            None,
            2_000,
        )
        .await
        .expect("short min");

    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 2);
    assert!(session.user_loaded(UserId(66)));
    assert_eq!(
        session.peer_access_hash(PeerId::from_user(UserId(66))),
        Some(6600)
    );

    // 3. Now user 66 is fully loaded -> next UpdateShortMessage applies directly (no getDifference)!
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                id: 303,
                user_id: 66,
                message: "direct short message".into(),
                pts: 13,
                pts_count: 1,
                date: 1_700_000_030,
                fwd_from: None,
                via_bot_id: None,
                reply_to: None,
                entities: None,
                ttl_period: None,
            }),
            None,
            2_500,
        )
        .await
        .expect("short direct");

    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 2);
    assert_eq!(handler.pts(), 13);
    assert!(
        session
            .message(FullMsgId::new(PeerId::from_user(UserId(66)), MsgId(303)))
            .is_some()
    );
}

#[tokio::test]
async fn short_sent_message_updates_promoted_message_date_entities_and_ttl() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    let u1 = PeerId::from_user(UserId(1));

    let local_id = MsgId::client_by_index(0).expect("client msg id");
    let mut local_msg = Message::new_text(
        u1,
        local_id,
        Some(PeerId::from_user(UserId(1000))),
        1_700_000_000,
        TextWithEntities::plain("bold hi"),
    );
    local_msg
        .flags
        .insert(MessageFlags::OUTGOING | MessageFlags::BEING_SENT);
    session.insert_message(local_msg);
    session.register_random_id(4242, FullMsgId::new(u1, local_id));

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        0,
        1_700_000_000,
        1,
    )));
    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");

    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
                out: true,
                id: 900,
                pts: 11,
                pts_count: 1,
                date: 1_700_000_999,
                media: None,
                entities: Some(vec![tl_enums::MessageEntity::Bold(
                    tl_types::MessageEntityBold {
                        offset: 0,
                        length: 4,
                    },
                )]),
                ttl_period: Some(86_400),
            }),
            Some(4242),
            1_500,
        )
        .await
        .expect("short sent");

    let promoted = session
        .message(FullMsgId::new(u1, MsgId(900)))
        .expect("promoted message");
    assert_eq!(promoted.date, 1_700_000_999);
    assert_eq!(promoted.ttl_period, Some(86_400));
    assert!(!promoted.flags.contains(MessageFlags::BEING_SENT));
    assert_eq!(promoted.text().expect("text").entities.len(), 1);
    assert_eq!(handler.pts(), 11);
}

#[tokio::test]
async fn min_channel_in_new_channel_message_arms_min_channel_timer_and_holds_date() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));

    // Seed channel 99 as `min: true` -> LoadedStatus::Minimal (not fully loaded).
    let mut min_ch = sample_tl_channel(99, 0, "MinChannel", false);
    min_ch.min = true;
    min_ch.access_hash = None;
    apply_tl_chat(&mut session, &tl_enums::Chat::Channel(min_ch));
    assert!(!session.channel_loaded(ChannelId(99)));

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        0,
        1_700_000_000,
        1,
    )));
    client.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Empty(
        tl_types::updates::DifferenceEmpty {
            date: 1_700_000_050,
            seq: 1,
        },
    ));

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");

    // Push UpdateNewChannelMessage for min channel -> arms 1s _byMinChannelTimer.
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                    message: sample_tl_message(channel_peer(99), None, 10, 1_700_000_020, "hi"),
                    pts: 10,
                    pts_count: 1,
                }),
                date: 1_700_000_020,
            }),
            None,
            2_000,
        )
        .await
        .expect("min channel msg");

    // Date must NOT have advanced to 1_700_000_020 while _byMinChannelTimer is active!
    assert_eq!(handler.state_snapshot().0, 1_700_000_000);
    assert_eq!(
        handler.next_deadline(),
        Some(2_000 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
    );

    // At +1000ms, fires getDifference with the held earlier date (1_700_000_000).
    handler
        .on_tick(&client, &mut session, 2_000 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
        .await
        .expect("min channel tick");
    let diffs = client.calls_of::<tl_fns::updates::GetDifference>();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].1.date, 1_700_000_000);
}

#[tokio::test]
async fn duplicate_updates_are_deduplicated_across_pts_gap_seq_and_qts() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    apply_tl_user(
        &mut session,
        &tl_enums::User::User(sample_tl_user(1, 10, "Alpha", false)),
    );
    let u1 = PeerId::from_user(UserId(1));

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        10,
        5,
        1_700_000_000,
        2,
    )));
    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .expect("init");

    // 1. Apply pts=11, then replay pts=11 (stale duplicate) -> ignored, original text preserved.
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 111, 1_700_000_011, "original 11", 11, 1),
            None,
            1_100,
        )
        .await
        .expect("pts 11");
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 111, 1_700_000_011, "mutated dup 11", 11, 1),
            None,
            1_200,
        )
        .await
        .expect("dup pts 11");
    assert_eq!(
        session
            .message(FullMsgId::new(u1, MsgId(111)))
            .expect("msg 111")
            .text()
            .expect("text")
            .text,
        "original 11"
    );

    // 2. Open gap at pts=13 (missing 12), replay pts=13 duplicate -> must NOT close gap!
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 113, 1_700_000_013, "original 13", 13, 1),
            None,
            1_300,
        )
        .await
        .expect("pts 13");
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 113, 1_700_000_013, "dup 13", 13, 1),
            None,
            1_350,
        )
        .await
        .expect("dup pts 13");
    assert_eq!(handler.pts(), 11);
    assert!(session.message(FullMsgId::new(u1, MsgId(113))).is_none());

    // Fill with pts=12 -> applies 12 then original 13.
    handler
        .handle_updates(
            &client,
            &mut session,
            new_msg_update(1, 112, 1_700_000_012, "original 12", 12, 1),
            None,
            1_400,
        )
        .await
        .expect("pts 12");
    assert_eq!(handler.pts(), 13);
    assert_eq!(
        session
            .message(FullMsgId::new(u1, MsgId(113)))
            .expect("msg 113")
            .text()
            .expect("text")
            .text,
        "original 13"
    );

    // 3. Stale seq <= 2 skipped.
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                    message: sample_tl_message(
                        user_peer(1),
                        Some(user_peer(1)),
                        199,
                        1_700_000_099,
                        "stale seq",
                    ),
                    pts: 14,
                    pts_count: 1,
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_099,
                seq: 2,
            }),
            None,
            1_500,
        )
        .await
        .expect("stale seq");
    assert_eq!(handler.pts(), 13);
    assert!(session.message(FullMsgId::new(u1, MsgId(199))).is_none());

    // 4. Idle check advances timestamp once per 60s window.
    assert!(!handler.check_last_update(false, 1_500 + NO_UPDATES_TIMEOUT_MS));
    assert!(handler.check_last_update(false, 1_500 + NO_UPDATES_TIMEOUT_MS + 1));
    assert!(!handler.check_last_update(false, 1_500 + NO_UPDATES_TIMEOUT_MS + 10));
}

#[tokio::test]
async fn resume_from_persisted_state_and_fallback_on_pts_expired() {
    // 1. Export state from first handler instance.
    let saved = PersistedUpdatesState {
        pts: 40,
        qts: 7,
        date: 1_700_001_000,
        seq: 12,
        channel_pts: [(ChannelId(88), 25)].into_iter().collect(),
    };

    // 2. Restart with saved state -> calls getDifference(pts=40, date=1_700_001_000, qts=7), NO getState!
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    client.enqueue_ok::<tl_fns::updates::GetDifference>(
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: vec![sample_tl_message(
                user_peer(1),
                Some(user_peer(1)),
                401,
                1_700_001_050,
                "missed while offline",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(sample_tl_user(1, 10, "Alpha", false))],
            state: tl_enums::updates::State::State(tl_state(42, 8, 1_700_001_050, 15)),
        }),
    );

    let mut resumed = UpdatesHandler::new();
    let used_diff = resumed
        .resume_from_state(&client, &mut session, &saved, 1_000)
        .await
        .expect("resume");
    assert!(used_diff);
    assert!(client.calls_of::<tl_fns::updates::GetState>().is_empty());
    let diffs = client.calls_of::<tl_fns::updates::GetDifference>();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].1.pts, 40);
    assert_eq!(diffs[0].1.date, 1_700_001_000);
    assert_eq!(diffs[0].1.qts, 7);
    assert_eq!(resumed.pts(), 42);
    assert_eq!(resumed.channel_pts(ChannelId(88)), Some(25));
    assert!(
        session
            .message(FullMsgId::new(PeerId::from_user(UserId(1)), MsgId(401)))
            .is_some()
    );

    // 3. Fallback when server rejects expired saved state with 400 PTS_EXPIRED -> calls getState!
    let client2 = FakeRpcClient::new(2);
    let mut session2 = Session::new(UserId(1000));
    client2.enqueue_err::<tl_fns::updates::GetDifference>(rpc_err(400, "PTS_EXPIRED"));
    client2.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        999,
        10,
        1_700_099_000,
        1,
    )));

    let mut fallback_handler = UpdatesHandler::new();
    let used_diff2 = fallback_handler
        .resume_from_state(&client2, &mut session2, &saved, 2_000)
        .await
        .expect("fallback");
    assert!(!used_diff2);
    assert_eq!(client2.calls_of::<tl_fns::updates::GetState>().len(), 1);
    assert_eq!(fallback_handler.pts(), 999);
}
