//! Integration tests for dialogs pagination, history slices, messaging,
//! read throttling, typing actions, drafts, and `UpdatesHandler` + `tg_data::Session`.

mod common;

use std::time::{Duration, Instant};

use common::{
    FakeRpcClient, SeqRandomIdSource, rpc_err, sample_tl_channel, sample_tl_dialog,
    sample_tl_message, sample_tl_user,
};
use tg_api::dialogs::{DIALOGS_FIRST_LOAD, DIALOGS_PER_PAGE, DialogsLoader, LOAD_EXCEPTIONS_AFTER};
use tg_api::error::SendMessageErrorKind;
use tg_api::history::{HISTORY_LIMIT, HistoryLoader, prepare_history_request};
use tg_api::messaging::{
    DeleteHistoryOutcome, DraftManager, ForwardItem, MessageSender, PendingMessageState,
    READ_REQUEST_TIMEOUT, ReadHistoryManager, SAVE_CLOUD_DRAFT_TIMEOUT, SEND_MY_SPEAKING_INTERVAL,
    SEND_MY_TYPING_INTERVAL, SendProgressKind, SendProgressManager, SendProgressUpdate,
    TypingPeerInfo,
};
use tg_api::types::{ForwardOptions, LoadDirection, SendOptions};
use tg_api::updates::{
    CHANNEL_GET_DIFF_LIMIT, NO_UPDATES_TIMEOUT_MS, UpdateEvent, UpdatesHandler,
    WAIT_FOR_SKIPPED_TIMEOUT_MS,
};
use tg_data::{
    ARCHIVE_FOLDER_ID, ChannelId, ChatId, FullMsgId, Message, MessageFlags, MsgId, MsgRange,
    PeerId, SERVER_MAX_MSG_ID, SHOW_AT_UNREAD_MSG_ID, Session, TextWithEntities, TypingAction,
    UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

fn user_input(id: i64) -> tl_enums::InputPeer {
    tl_enums::InputPeer::User(tl_types::InputPeerUser {
        user_id: id,
        access_hash: id * 10,
    })
}

fn channel_input(id: i64) -> tl_enums::InputPeer {
    tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
        channel_id: id,
        access_hash: id * 100,
    })
}

fn user_peer(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser { user_id: id })
}

fn channel_peer(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: id })
}

fn empty_state(pts: i32, qts: i32, date: i32, seq: i32) -> tl_types::updates::State {
    tl_types::updates::State {
        pts,
        qts,
        date,
        seq,
        unread_count: 0,
    }
}

#[tokio::test]
async fn dialogs_pagination_pinned_archive_filters_and_session_apply() {
    let client = FakeRpcClient::new(2);
    let u1_tl = sample_tl_user(1, 10, "Alpha", false);
    let u2_tl = sample_tl_user(2, 20, "Beta", false);
    let u3_tl = sample_tl_user(3, 30, "Gamma", false);

    // Page 1 (Slice) + pinned dialogs.
    client.enqueue_ok::<tl_fns::messages::GetDialogs>(&tl_enums::messages::Dialogs::Slice(
        tl_types::messages::DialogsSlice {
            count: 3,
            dialogs: vec![
                sample_tl_dialog(user_peer(1), 100),
                sample_tl_dialog(user_peer(2), 90),
            ],
            messages: vec![
                sample_tl_message(user_peer(1), Some(user_peer(1)), 100, 1_700_000_200, "hi 1"),
                sample_tl_message(user_peer(2), Some(user_peer(2)), 90, 1_700_000_100, "hi 2"),
            ],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(u1_tl), tl_enums::User::User(u2_tl)],
        },
    ));
    client.enqueue_ok::<tl_fns::messages::GetPinnedDialogs>(
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(empty_state(1, 0, 1_700_000_200, 1)),
        }),
    );
    // Page 2 (Complete).
    client.enqueue_ok::<tl_fns::messages::GetDialogs>(&tl_enums::messages::Dialogs::Dialogs(
        tl_types::messages::Dialogs {
            dialogs: vec![sample_tl_dialog(user_peer(3), 50)],
            messages: vec![sample_tl_message(
                user_peer(3),
                Some(user_peer(3)),
                50,
                1_700_000_050,
                "hi 3",
            )],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(u3_tl)],
        },
    ));

    // Filters + exception lookup + reorder pinned + archive.
    let u_exc = user_input(99);
    client.enqueue_ok::<tl_fns::messages::GetDialogFilters>(
        &tl_enums::messages::DialogFilters::DialogFilters(tl_types::messages::DialogFilters {
            tags_enabled: true,
            filters: vec![tl_enums::DialogFilter::DialogFilter(
                tl_types::DialogFilter {
                    contacts: false,
                    non_contacts: false,
                    groups: false,
                    broadcasts: false,
                    bots: false,
                    exclude_muted: false,
                    exclude_read: false,
                    exclude_archived: false,
                    title_noanimate: false,
                    id: 2,
                    title: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Work".into(),
                            entities: Vec::new(),
                        },
                    ),
                    emoticon: None,
                    color: Some(1),
                    pinned_peers: Vec::new(),
                    include_peers: vec![u_exc],
                    exclude_peers: Vec::new(),
                },
            )],
        }),
    );
    client.enqueue_ok::<tl_fns::messages::GetPeerDialogs>(
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: vec![sample_tl_dialog(user_peer(99), 7)],
            messages: vec![sample_tl_message(
                user_peer(99),
                Some(user_peer(99)),
                7,
                1_700_000_001,
                "exc",
            )],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(sample_tl_user(99, 990, "Exc", false))],
            state: tl_enums::updates::State::State(empty_state(1, 0, 1_700_000_200, 1)),
        }),
    );
    client.enqueue_ok::<tl_fns::messages::ReorderPinnedDialogs>(&true);
    client.enqueue_ok::<tl_fns::folders::EditPeerFolders>(&tl_enums::Updates::Updates(
        tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_700_000_300,
            seq: 0,
        },
    ));

    let mut session = Session::new(UserId(1000));
    let mut loader = DialogsLoader::new();

    // Notice: we do NOT pre-register peer inputs — DialogsLoader extracts access_hash from Page 1's users!
    let p1 = loader.load_next_page(&client, 0).await.unwrap().unwrap();
    assert_eq!(p1.dialogs.len(), 2);
    assert!(p1.pinned.is_some());
    assert!(!p1.folder_done);
    p1.apply_to_session(&mut session, 1_700_000_250);
    assert!(session.user(UserId(1)).is_some());
    assert_eq!(
        session.peer_access_hash(PeerId::from_user(UserId(2))),
        Some(20)
    );

    let p2 = loader.load_next_page(&client, 0).await.unwrap().unwrap();
    assert_eq!(p2.dialogs.len(), 1);
    assert!(p2.pinned.is_none());
    assert!(p2.folder_done);
    assert!(loader.is_folder_done(0));
    p2.apply_to_session(&mut session, 1_700_000_250);
    assert!(
        session
            .message(FullMsgId::new(PeerId::from_user(UserId(3)), MsgId(50)))
            .is_some()
    );

    // Third call returns None (already finished).
    assert!(loader.load_next_page(&client, 0).await.unwrap().is_none());

    // Verify exact pagination parameters sent on the wire to messages.getDialogs.
    let dialog_reqs = client.calls_of::<tl_fns::messages::GetDialogs>();
    assert_eq!(dialog_reqs.len(), 2);
    assert_eq!(dialog_reqs[0].1.limit, DIALOGS_FIRST_LOAD);
    assert!(dialog_reqs[0].1.exclude_pinned);
    assert_eq!(dialog_reqs[0].1.folder_id, Some(0));
    assert_eq!(dialog_reqs[0].1.offset_date, 0);
    assert_eq!(dialog_reqs[1].1.limit, DIALOGS_PER_PAGE);
    assert_eq!(dialog_reqs[1].1.offset_date, 1_700_000_100);
    assert_eq!(dialog_reqs[1].1.offset_id, 90);
    assert_eq!(dialog_reqs[1].1.offset_peer, user_input(2));

    // Filters, archive_needed, apply_filters_to_session, and filter exception loading.
    loader.load_dialog_filters(&client).await.unwrap();
    assert!(loader.archive_needed());
    loader.apply_filters_to_session(&mut session);
    assert!(session.filters().tags_enabled());
    assert!(session.filters().get(2).is_some());
    let exc = loader
        .load_next_filter_exceptions(&client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exc.dialogs.len(), 1);
    assert_eq!(LOAD_EXCEPTIONS_AFTER, 100);

    // Reorder pinned & archive.
    assert!(
        loader
            .save_pinned_order(
                &client,
                0,
                vec![tl_enums::InputDialogPeer::InputDialogPeer(
                    tl_types::InputDialogPeer {
                        peer: user_input(1),
                    }
                )]
            )
            .await
            .unwrap()
    );
    loader
        .toggle_history_archived(&client, user_input(1), true)
        .await
        .unwrap();
    let edit_folders = client.calls_of::<tl_fns::folders::EditPeerFolders>();
    assert_eq!(edit_folders.len(), 1);
    match &edit_folders[0].1.folder_peers[0] {
        tl_enums::InputFolderPeer::InputFolderPeer(p) => assert_eq!(p.folder_id, ARCHIVE_FOLDER_ID),
    }
}

#[tokio::test]
async fn history_loader_slices_unread_and_jump_to_date() {
    let client = FakeRpcClient::new(2);
    let peer = channel_input(500);
    let legacy = tl_enums::InputPeer::Chat(tl_types::InputPeerChat { chat_id: 7 });

    // Verify exact request offset/add_offset calculation.
    let req_before = prepare_history_request(peer.clone(), MsgId(200), LoadDirection::Before);
    assert_eq!(req_before.offset_id, 200);
    assert_eq!(req_before.add_offset, 0);
    assert_eq!(req_before.limit, HISTORY_LIMIT);

    let req_around = prepare_history_request(peer.clone(), MsgId(200), LoadDirection::Around);
    assert_eq!(req_around.offset_id, 200);
    assert_eq!(req_around.add_offset, -25);

    let req_after = prepare_history_request(peer.clone(), MsgId(200), LoadDirection::After);
    assert_eq!(req_after.offset_id, 201);
    assert_eq!(req_after.add_offset, -50);

    // Queue empty After slice to verify no_skip_range boundary expansion.
    client.enqueue_ok::<tl_fns::messages::GetHistory>(
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 42,
            count: 200,
            offset_id_offset: None,
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    // Queue jump-to-date: legacy chat returns empty, supergroup returns msg at target date.
    client.enqueue_ok::<tl_fns::messages::GetHistory>(&tl_enums::messages::Messages::Messages(
        tl_types::messages::Messages {
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        },
    ));
    client.enqueue_ok::<tl_fns::messages::GetHistory>(
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 43,
            count: 200,
            offset_id_offset: None,
            messages: vec![sample_tl_message(
                channel_peer(500),
                None,
                155,
                1_700_005_000,
                "target",
            )],
            topics: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(sample_tl_channel(
                500, 50000, "News", false,
            ))],
            users: Vec::new(),
        }),
    );
    // Queue jump-to-date fallback when no message matches -> SHOW_AT_UNREAD_MSG_ID.
    client.enqueue_ok::<tl_fns::messages::GetHistory>(&tl_enums::messages::Messages::Messages(
        tl_types::messages::Messages {
            messages: vec![sample_tl_message(
                channel_peer(500),
                None,
                10,
                1_600_000_000,
                "too old",
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        },
    ));

    let mut loader = HistoryLoader::new();
    let slice = loader
        .request_history(&client, peer.clone(), MsgId(200), LoadDirection::After)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slice.channel_pts, Some(42));
    assert_eq!(
        slice.no_skip_range,
        MsgRange::new(MsgId(200), SERVER_MAX_MSG_ID)
    );

    let mut session = Session::new(UserId(1));
    slice.apply_to_session(&mut session, PeerId::from_channel(ChannelId(500)));

    let jumped = loader
        .resolve_jump_to_time(&client, peer.clone(), Some(legacy), None, 1_700_005_000)
        .await
        .unwrap();
    assert_eq!(jumped.peer, PeerId::from_channel(ChannelId(500)));
    assert_eq!(jumped.msg_id, MsgId(155));

    let fallback = loader
        .resolve_jump_to_time(&client, peer.clone(), None, None, 1_700_005_000)
        .await
        .unwrap();
    assert_eq!(fallback.msg_id, SHOW_AT_UNREAD_MSG_ID);

    let hist_reqs = client.calls_of::<tl_fns::messages::GetHistory>();
    assert_eq!(hist_reqs[1].1.offset_date, 1_700_004_999);
    assert_eq!(hist_reqs[1].1.add_offset, -1);
    assert_eq!(hist_reqs[1].1.limit, 1);
}

#[tokio::test]
async fn send_message_success_split_failure_edit_delete_and_forward() {
    let client = FakeRpcClient::new(2);
    let peer = user_input(10);
    let peer2 = user_input(20);
    let ch = channel_input(77);

    // 1. Send message split into 2 chunks: first returns UpdateMessageID, second returns ShortSentMessage.
    client.enqueue_ok::<tl_fns::messages::SendMessage>(&tl_enums::Updates::Updates(
        tl_types::Updates {
            updates: vec![tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                id: 501,
                random_id: 1000,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_700_000_001,
            seq: 1,
        },
    ));
    client.enqueue_ok::<tl_fns::messages::SendMessage>(&tl_enums::Updates::UpdateShortSentMessage(
        tl_types::UpdateShortSentMessage {
            out: true,
            id: 502,
            pts: 10,
            pts_count: 1,
            date: 1_700_000_002,
            media: None,
            entities: None,
            ttl_period: None,
        },
    ));
    // 2. Send failures: PEER_FLOOD -> Failed, MESSAGE_EMPTY -> Destroyed.
    client.enqueue_err::<tl_fns::messages::SendMessage>(rpc_err(400, "PEER_FLOOD"));
    client.enqueue_err::<tl_fns::messages::SendMessage>(rpc_err(400, "MESSAGE_EMPTY"));
    // 3. Edit, delete (channel & non-channel), deleteHistory pagination loop, forward batching.
    client.enqueue_ok::<tl_fns::messages::EditMessage>(&tl_enums::Updates::Updates(
        tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_700_000_003,
            seq: 2,
        },
    ));
    client.enqueue_ok::<tl_fns::channels::DeleteMessages>(
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 5,
                pts_count: 1,
            },
        ),
    );
    client.enqueue_ok::<tl_fns::messages::DeleteMessages>(
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 11,
                pts_count: 1,
            },
        ),
    );
    client.enqueue_ok::<tl_fns::messages::DeleteHistory>(
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 12,
                pts_count: 1,
                offset: 50,
            },
        ),
    );
    client.enqueue_ok::<tl_fns::messages::DeleteHistory>(
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 13,
                pts_count: 1,
                offset: 0,
            },
        ),
    );
    for seq in [3, 4] {
        client.enqueue_ok::<tl_fns::messages::ForwardMessages>(&tl_enums::Updates::Updates(
            tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_004,
                seq,
            },
        ));
    }

    let mut sender = MessageSender::new(SeqRandomIdSource::new(1000));
    sender.set_message_length_limit(5);

    let outcome = sender
        .send_message(
            &client,
            peer.clone(),
            "helloworld",
            Vec::new(),
            None,
            SendOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.messages.len(), 2);
    assert!(outcome.messages[0].local_id.msg.is_client());
    assert_eq!(
        outcome.messages[0].state,
        PendingMessageState::Sent {
            server_id: MsgId(501)
        }
    );
    assert_eq!(
        outcome.messages[1].state,
        PendingMessageState::Sent {
            server_id: MsgId(502)
        }
    );
    assert!(sender.message_id_by_random_id(1000).is_none());

    // Failure -> Failed state.
    let err = sender
        .send_message(
            &client,
            peer.clone(),
            "spam",
            Vec::new(),
            None,
            SendOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err, SendMessageErrorKind::PeerFlood);
    let local_3 = FullMsgId::new(
        PeerId::from_user(UserId(10)),
        MsgId::client_by_index(2).unwrap(),
    );
    assert_eq!(
        sender.pending_message(local_3).unwrap().state,
        PendingMessageState::Failed {
            error: SendMessageErrorKind::PeerFlood
        }
    );

    // MESSAGE_EMPTY -> Destroyed state.
    let err2 = sender
        .send_message(
            &client,
            peer.clone(),
            "   ",
            Vec::new(),
            None,
            SendOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err2, SendMessageErrorKind::MessageEmpty);
    let local_4 = FullMsgId::new(
        PeerId::from_user(UserId(10)),
        MsgId::client_by_index(3).unwrap(),
    );
    assert_eq!(
        sender.pending_message(local_4).unwrap().state,
        PendingMessageState::Destroyed
    );

    // Edit & Delete & DeleteHistory loop.
    sender
        .edit_message(
            &client,
            tl_fns::messages::EditMessage {
                no_webpage: false,
                invert_media: false,
                peer: peer.clone(),
                id: 501,
                message: Some("edited".into()),
                media: None,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                quick_reply_shortcut_id: None,
                rich_message: None,
            },
        )
        .await
        .unwrap();
    let ch_del = sender
        .delete_messages(&client, ch.clone(), vec![MsgId(1)], false)
        .await
        .unwrap();
    assert_eq!(ch_del.pts, 5);
    let dm_del = sender
        .delete_messages(&client, peer.clone(), vec![MsgId(501)], true)
        .await
        .unwrap();
    assert_eq!(dm_del.pts, 11);

    let del_hist = sender
        .delete_history(
            &client,
            tl_fns::messages::DeleteHistory {
                just_clear: true,
                revoke: true,
                peer: peer.clone(),
                max_id: 0,
                min_date: None,
                max_date: None,
            },
        )
        .await
        .unwrap();
    match del_hist {
        DeleteHistoryOutcome::Affected(pages) => assert_eq!(pages.len(), 2),
        other => panic!("expected Affected, got {other:?}"),
    }

    // Forward 3 items from 2 different source peers -> 2 RPC batches.
    let fwd = sender
        .forward_messages(
            &client,
            &[
                ForwardItem {
                    from_peer: peer.clone(),
                    msg_id: MsgId(10),
                },
                ForwardItem {
                    from_peer: peer.clone(),
                    msg_id: MsgId(11),
                },
                ForwardItem {
                    from_peer: peer2.clone(),
                    msg_id: MsgId(20),
                },
            ],
            ch,
            None,
            ForwardOptions::NoSenderNames,
            SendOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(fwd.updates.len(), 2);
}

#[tokio::test]
async fn read_history_throttling_typing_and_drafts() {
    let client = FakeRpcClient::new(2);
    let peer = user_input(33);
    let ch = channel_input(44);
    let t0 = Instant::now();

    // --- Read throttling ---
    client.enqueue_ok::<tl_fns::messages::ReadHistory>(
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 100,
                pts_count: 1,
            },
        ),
    );
    client.enqueue_ok::<tl_fns::channels::ReadHistory>(&true);

    let mut reads = ReadHistoryManager::new();
    assert!(!reads.read_inbox_till(peer.clone(), MsgId(10), Some(5), false, t0));
    assert!(!reads.read_inbox_till(
        peer.clone(),
        MsgId(12),
        Some(3),
        false,
        t0 + Duration::from_secs(1)
    ));
    assert!(
        reads
            .flush_due(&client, t0 + Duration::from_secs(2))
            .await
            .unwrap()
            .is_empty()
    );
    let flushed = reads
        .flush_due(&client, t0 + READ_REQUEST_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].max_id, MsgId(12));
    assert_eq!(flushed[0].affected.as_ref().unwrap().pts, 100);

    // Deduplicated repeat.
    assert!(!reads.read_inbox_till(
        peer.clone(),
        MsgId(12),
        Some(0),
        true,
        t0 + Duration::from_secs(4)
    ));

    // Channel with still_unread == 0 flushes immediately.
    assert!(reads.read_inbox_till(ch.clone(), MsgId(77), Some(0), false, t0));
    let flushed_ch = reads.flush_due(&client, t0).await.unwrap();
    assert_eq!(flushed_ch.len(), 1);
    assert!(flushed_ch[0].affected.is_none());

    // --- Typing manager ---
    for _ in 0..3 {
        client.enqueue_ok::<tl_fns::messages::SetTyping>(&true);
    }
    let mut typing = SendProgressManager::new();
    let offline_peer = user_input(34);
    let online_info = TypingPeerInfo::online_user(PeerId::from_user(UserId(33)), 1_700_000_100);
    let offline_info = TypingPeerInfo::online_user(PeerId::from_user(UserId(34)), 1_700_000_000);
    // Offline > 30s -> skipped.
    assert!(
        !typing
            .update(
                &client,
                SendProgressUpdate {
                    peer: offline_peer,
                    info: offline_info,
                    top_msg_id: None,
                    kind: SendProgressKind::Typing,
                    progress: 0,
                },
                t0,
                1_700_000_050
            )
            .await
            .unwrap()
    );
    // Online -> sent & throttled for 5s.
    assert!(
        typing
            .update(
                &client,
                SendProgressUpdate {
                    peer: peer.clone(),
                    info: online_info,
                    top_msg_id: None,
                    kind: SendProgressKind::Typing,
                    progress: 0,
                },
                t0,
                1_700_000_050
            )
            .await
            .unwrap()
    );
    assert!(
        !typing
            .update(
                &client,
                SendProgressUpdate {
                    peer: peer.clone(),
                    info: online_info,
                    top_msg_id: None,
                    kind: SendProgressKind::Typing,
                    progress: 0,
                },
                t0 + Duration::from_secs(4),
                1_700_000_054
            )
            .await
            .unwrap()
    );
    assert!(
        typing
            .update(
                &client,
                SendProgressUpdate {
                    peer: peer.clone(),
                    info: online_info,
                    top_msg_id: None,
                    kind: SendProgressKind::Typing,
                    progress: 0,
                },
                t0 + SEND_MY_TYPING_INTERVAL,
                1_700_000_055
            )
            .await
            .unwrap()
    );
    // Speaking interval is 3s.
    let group_info = TypingPeerInfo::group(PeerId::from_channel(ChannelId(44)));
    assert!(
        typing
            .update(
                &client,
                SendProgressUpdate {
                    peer: ch.clone(),
                    info: group_info,
                    top_msg_id: None,
                    kind: SendProgressKind::Speaking,
                    progress: 0,
                },
                t0,
                1_700_000_050
            )
            .await
            .unwrap()
    );
    assert!(
        !typing
            .update(
                &client,
                SendProgressUpdate {
                    peer: ch.clone(),
                    info: group_info,
                    top_msg_id: None,
                    kind: SendProgressKind::Speaking,
                    progress: 0,
                },
                t0 + SEND_MY_SPEAKING_INTERVAL - Duration::from_millis(100),
                1_700_000_052
            )
            .await
            .unwrap()
    );

    // --- Drafts save with 1s delay & quit prevention ---
    client.enqueue_ok::<tl_fns::messages::SaveDraft>(&true);
    let mut drafts = DraftManager::new();
    assert!(!drafts.is_quit_prevent());
    drafts.schedule_save(
        None,
        tl_fns::messages::SaveDraft {
            no_webpage: false,
            invert_media: false,
            reply_to: None,
            peer: peer.clone(),
            message: "draft text".into(),
            entities: None,
            media: None,
            effect: None,
            suggested_post: None,
            rich_message: None,
        },
        t0,
    );
    assert!(drafts.is_quit_prevent());
    assert_eq!(
        drafts
            .flush_due(&client, t0 + Duration::from_millis(500))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        drafts
            .flush_due(&client, t0 + SAVE_CLOUD_DRAFT_TIMEOUT)
            .await
            .unwrap(),
        1
    );
    assert!(!drafts.is_quit_prevent());
}

#[tokio::test]
async fn updates_handler_session_mutations_pts_gap_channel_diff_stream_and_force_logout() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1000));
    let u1_pid = PeerId::from_user(UserId(1));
    let ch_id = ChannelId(88);
    let ch_pid = PeerId::from_channel(ch_id);
    session.set_peer_access_hash(ch_pid, 8800);

    // Pre-insert a local outgoing message to verify UpdateMessageID resolution in Session.
    let local_id = MsgId::client_by_index(0).unwrap();
    let mut local_msg = Message::new_text(
        u1_pid,
        local_id,
        Some(PeerId::from_user(UserId(1000))),
        1_700_000_000,
        TextWithEntities::plain("pending hello"),
    );
    local_msg.flags.insert(MessageFlags::OUTGOING);
    session.insert_message(local_msg);
    session.register_random_id(777, FullMsgId::new(u1_pid, local_id));

    // 1. Initial getState -> pts = 10, seq = 5.
    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(empty_state(
        10,
        0,
        1_700_000_000,
        5,
    )));
    // 2. getDifference returns Slice (pts 11) then Full (pts 12), with UpdateMessageID(777 -> 101).
    client.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Slice(
        tl_types::updates::DifferenceSlice {
            new_messages: vec![sample_tl_message(
                user_peer(1),
                Some(user_peer(1)),
                101,
                1_700_000_010,
                "missed 1",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: vec![tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                id: 101,
                random_id: 777,
            })],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(sample_tl_user(1, 10, "Alpha", false))],
            intermediate_state: tl_enums::updates::State::State(empty_state(
                11,
                0,
                1_700_000_010,
                6,
            )),
        },
    ));
    client.enqueue_ok::<tl_fns::updates::GetDifference>(
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: vec![sample_tl_message(
                user_peer(1),
                Some(user_peer(1)),
                102,
                1_700_000_011,
                "missed 2",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: vec![tl_enums::Update::UserTyping(tl_types::UpdateUserTyping {
                user_id: 1,
                top_msg_id: None,
                action: tl_enums::SendMessageAction::SendMessageTypingAction,
            })],
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(empty_state(12, 0, 1_700_000_011, 7)),
        }),
    );
    // 3. Channel difference multi-page (!final -> TooLong).
    client.enqueue_ok::<tl_fns::updates::GetChannelDifference>(
        &tl_enums::updates::ChannelDifference::ChannelDifference(
            tl_types::updates::ChannelDifference {
                r#final: false,
                pts: 20,
                timeout: Some(15),
                new_messages: vec![sample_tl_message(
                    channel_peer(88),
                    None,
                    1,
                    1_700_000_100,
                    "ch1",
                )],
                other_updates: Vec::new(),
                chats: vec![tl_enums::Chat::Channel(sample_tl_channel(
                    88, 8800, "News", false,
                ))],
                users: Vec::new(),
            },
        ),
    );
    client.enqueue_ok::<tl_fns::updates::GetChannelDifference>(
        &tl_enums::updates::ChannelDifference::TooLong(
            tl_types::updates::ChannelDifferenceTooLong {
                r#final: true,
                timeout: Some(30),
                dialog: sample_tl_dialog(channel_peer(88), 25),
                messages: vec![sample_tl_message(
                    channel_peer(88),
                    None,
                    25,
                    1_700_000_200,
                    "latest ch",
                )],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    // Follow-up range difference (`requestChannelRangeDifference` in `api/api_updates.cpp:797-890`)
    // validating already-loaded message [1..=1] after `channelDifferenceTooLong`.
    client.enqueue_ok::<tl_fns::updates::GetChannelDifference>(
        &tl_enums::updates::ChannelDifference::Empty(tl_types::updates::ChannelDifferenceEmpty {
            r#final: true,
            pts: 25,
            timeout: None,
        }),
    );
    // 4. Online presence.
    client.enqueue_ok::<tl_fns::account::UpdateStatus>(&true);

    let mut handler = UpdatesHandler::new();

    // AUTH_KEY_DROP_* must be surfaced even BEFORE initialize (while requesting_difference == true).
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ServiceNotification(
                    tl_types::UpdateServiceNotification {
                        popup: false,
                        invert_media: false,
                        inbox_date: None,
                        r#type: "AUTH_KEY_DROP_DUPLICATE".into(),
                        message: "Logout".into(),
                        media: tl_enums::MessageMedia::Empty,
                        entities: Vec::new(),
                    },
                ),
                date: 1_700_000_000,
            }),
            None,
            0,
        )
        .await
        .unwrap();
    let pre_events = handler.drain_events();
    assert_eq!(
        pre_events,
        vec![UpdateEvent::ForceLogout {
            notification_type: "AUTH_KEY_DROP_DUPLICATE".into(),
            message: "Logout".into(),
            entities: Vec::new(),
        }]
    );

    // Initialize via updates.getState.
    handler
        .initialize(&client, &mut session, 1_000)
        .await
        .unwrap();
    assert_eq!(handler.pts(), 10);

    // Push a PTS gap update (pts = 12, pts_count = 1) via the live broadcast stream!
    let mut rx = tg_api::RpcClient::subscribe_updates(&client).unwrap();
    client.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: sample_tl_message(
                user_peer(1),
                Some(user_peer(1)),
                102,
                1_700_000_011,
                "future",
            ),
            pts: 12,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_011,
        seq: 0,
    }));
    let pumped = handler
        .pump_updates_stream(&client, &mut session, &mut rx, 1_000)
        .await
        .unwrap();
    assert_eq!(pumped, 1);
    // Buffered — not applied yet.
    assert!(
        session
            .message(FullMsgId::new(u1_pid, MsgId(102)))
            .is_none()
    );

    // Advance time by WAIT_FOR_SKIPPED_TIMEOUT_MS -> fires getDifference (Slice -> Full).
    let need_ping = handler
        .on_tick(&client, &mut session, 1_000 + WAIT_FOR_SKIPPED_TIMEOUT_MS)
        .await
        .unwrap();
    assert!(!need_ping);
    assert_eq!(handler.pts(), 12);
    // Both missed messages & user & typing action applied into Session!
    assert!(
        session
            .message(FullMsgId::new(u1_pid, MsgId(101)))
            .is_some()
    );
    assert!(
        session
            .message(FullMsgId::new(u1_pid, MsgId(102)))
            .is_some()
    );
    assert_eq!(
        session.active_typing(u1_pid),
        vec![(u1_pid, None, TypingAction::Typing)]
    );
    let events = handler.drain_events();
    assert!(events.contains(&UpdateEvent::MessageIdMapped {
        random_id: 777,
        server_id: MsgId(101),
    }));

    // UpdateShortMessage when sender is loaded -> applied in-order directly to Session.
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                id: 103,
                user_id: 1,
                message: "short hi".into(),
                pts: 13,
                pts_count: 1,
                date: 1_700_000_020,
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
        .unwrap();
    assert_eq!(handler.pts(), 13);
    assert_eq!(
        session
            .message(FullMsgId::new(u1_pid, MsgId(103)))
            .unwrap()
            .text()
            .unwrap()
            .text,
        "short hi"
    );

    // UpdateShortChatMessage when chat is NOT loaded -> triggers getDifference!
    client.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Empty(
        tl_types::updates::DifferenceEmpty {
            date: 1_700_000_030,
            seq: 7,
        },
    ));
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShortChatMessage(tl_types::UpdateShortChatMessage {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                id: 104,
                from_id: 1,
                chat_id: 999,
                message: "unloaded group".into(),
                pts: 14,
                pts_count: 1,
                date: 1_700_000_025,
                fwd_from: None,
                via_bot_id: None,
                reply_to: None,
                entities: None,
                ttl_period: None,
            }),
            None,
            3_000,
        )
        .await
        .unwrap();
    assert!(session.chat(ChatId(999)).is_none());
    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 3);

    // Channel difference via UpdateChannelTooLong -> loops !final then TooLong!
    handler.init_channel_pts(ch_id, 19);
    handler
        .handle_updates(
            &client,
            &mut session,
            tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::ChannelTooLong(tl_types::UpdateChannelTooLong {
                    channel_id: 88,
                    pts: Some(25),
                }),
                date: 1_700_000_200,
            }),
            None,
            4_000,
        )
        .await
        .unwrap();
    let ch_diff_calls = client.calls_of::<tl_fns::updates::GetChannelDifference>();
    assert_eq!(ch_diff_calls.len(), 3);
    assert_eq!(ch_diff_calls[0].1.limit, CHANNEL_GET_DIFF_LIMIT);
    assert!(ch_diff_calls[2].1.force);
    assert!(matches!(
        ch_diff_calls[2].1.filter,
        tl_enums::ChannelMessagesFilter::ChannelMessagesFilter(_)
    ));
    assert!(session.message(FullMsgId::new(ch_pid, MsgId(25))).is_some());
    assert!(handler.drain_events().iter().any(
        |e| matches!(e, UpdateEvent::ChannelTooLong { channel_id, .. } if *channel_id == ch_id)
    ));

    // Watchdog signals ping strictly after 60s of silence (`now - last > NO_UPDATES_TIMEOUT_MS`).
    assert!(
        !handler
            .on_tick(&client, &mut session, 4_000 + NO_UPDATES_TIMEOUT_MS)
            .await
            .unwrap()
    );
    let ping_due = handler
        .on_tick(&client, &mut session, 4_000 + NO_UPDATES_TIMEOUT_MS + 1)
        .await
        .unwrap();
    assert!(ping_due);

    // Online presence updates account.updateStatus(offline = false).
    assert!(handler.update_online(&client, true).await.unwrap());
    assert!(handler.last_was_online());
    let status_calls = client.calls_of::<tl_fns::account::UpdateStatus>();
    assert_eq!(status_calls.len(), 1);
    assert!(!status_calls[0].1.offline);
}

#[tokio::test]
async fn updates_handler_get_difference_exponential_backoff_on_failure() {
    let client = FakeRpcClient::new(2);
    let mut session = Session::new(UserId(1));

    client.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(empty_state(
        1, 0, 100, 1,
    )));
    client.enqueue_err::<tl_fns::updates::GetDifference>(rpc_err(500, "INTERNAL"));
    client.enqueue_err::<tl_fns::updates::GetDifference>(rpc_err(500, "INTERNAL"));
    client.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Empty(
        tl_types::updates::DifferenceEmpty { date: 105, seq: 1 },
    ));

    let mut handler = UpdatesHandler::new();
    handler
        .initialize(&client, &mut session, 10_000)
        .await
        .unwrap();

    // Trigger getDifference via Updates::TooLong -> fails, schedules +1s retry (at 11_000).
    assert!(
        handler
            .handle_updates(
                &client,
                &mut session,
                tl_enums::Updates::TooLong,
                None,
                10_000
            )
            .await
            .is_err()
    );
    // At 10_500: not due yet.
    handler
        .on_tick(&client, &mut session, 10_500)
        .await
        .unwrap();
    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 1);

    // At 11_000: retries and fails -> schedules +2s retry (at 13_000).
    assert!(
        handler
            .on_tick(&client, &mut session, 11_000)
            .await
            .is_err()
    );
    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 2);

    // At 12_000: not due yet.
    handler
        .on_tick(&client, &mut session, 12_000)
        .await
        .unwrap();
    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 2);

    // At 13_000: retries and succeeds!
    handler
        .on_tick(&client, &mut session, 13_000)
        .await
        .unwrap();
    assert_eq!(client.calls_of::<tl_fns::updates::GetDifference>().len(), 3);
    assert_eq!(handler.state_snapshot().0, 105);
}
