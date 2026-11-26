//! View versions (`DialogsBackend::dialogs_version`, `HistoryBackend::chat_version`) and the
//! coalesced UI wake-up.

use super::*;
use tg_data::{ChannelId, FullMsgId, UserId};

fn user(id: u64) -> DataPeerId {
    DataPeerId::from_user(UserId(id))
}

#[test]
fn a_chat_change_moves_that_chat_and_the_list_but_not_other_chats() {
    let v = ViewVersions::default();
    let (a, b) = (user(1), user(2));
    let (a0, b0, list0) = (v.chat(a), v.chat(b), v.dialogs());
    v.bump_chat(a);
    assert!(v.chat(a) > a0);
    assert_eq!(v.chat(b), b0, "another chat keeps its version");
    assert!(v.dialogs() > list0, "the chat list shows every chat");
}

#[test]
fn a_shared_change_moves_every_chat() {
    let v = ViewVersions::default();
    let (a, b) = (user(1), user(2));
    v.bump_chat(a);
    let (a0, b0, list0) = (v.chat(a), v.chat(b), v.dialogs());
    v.bump_all();
    assert!(v.chat(a) > a0);
    assert!(v.chat(b) > b0);
    assert!(v.dialogs() > list0);
}

#[test]
fn versions_never_go_back_when_the_per_chat_table_is_folded() {
    let v = ViewVersions::default();
    let first = user(1);
    v.bump_chat(first);
    let mut last_seen = v.chat(first);
    for id in 2..(MAX_VERSIONED_CHATS as u64 + 10) {
        v.bump_chat(user(id));
        let now = v.chat(first);
        assert!(now >= last_seen, "chat version went back at {id}");
        last_seen = now;
    }
    let before = v.chat(first);
    v.bump_chat(first);
    assert!(v.chat(first) > before);
}

#[test]
fn change_events_name_their_chat_only_when_no_other_view_shows_them() {
    let a = user(1);
    let msg = ChangeEvent::MessageUpdated {
        id: FullMsgId::new(a, DataMsgId(5)),
        flags: MessageUpdateFlags::NEW_ADDED,
    };
    assert_eq!(change_event_chat(&msg), Some(a));
    let history = ChangeEvent::HistoryUpdated {
        peer_id: a,
        flags: HistoryUpdateFlags::UNREAD_VIEW,
    };
    assert_eq!(change_event_chat(&history), Some(a));
    // Names and statuses show in every chat that mentions the peer.
    let peer = ChangeEvent::PeerUpdated {
        peer_id: a,
        flags: tg_data::PeerUpdateFlags::NAME,
    };
    assert_eq!(change_event_chat(&peer), None);
    assert_eq!(change_event_chat(&ChangeEvent::FiltersChanged), None);
    let group = DataPeerId::from_channel(ChannelId(9));
    let entry = ChangeEvent::EntryUpdated {
        key: tg_data::DialogKey::History(group),
        flags: tg_data::EntryUpdateFlags::empty(),
    };
    assert_eq!(change_event_chat(&entry), Some(group));
}

#[test]
fn notify_ui_moves_every_version() {
    let state = LiveBridgeState::new();
    let a = user(1);
    let (chat0, list0, gen0) = (
        state.chat_version(a),
        state.dialogs_version(),
        state.generation(),
    );
    state.notify_ui();
    assert!(state.chat_version(a) > chat0);
    assert!(state.dialogs_version() > list0);
    assert!(state.generation() > gen0);
}

#[test]
fn a_chat_notification_keeps_other_chats_and_still_counts_as_a_generation() {
    let state = LiveBridgeState::new();
    let (a, b) = (user(1), user(2));
    let (b0, gen0) = (state.chat_version(b), state.generation());
    state.notify_chat_changed(a);
    assert_eq!(state.chat_version(b), b0);
    assert!(state.generation() > gen0);
}

#[test]
fn a_burst_of_changes_between_frames_requests_one_repaint() {
    let state = LiveBridgeState::new();
    let ctx = egui::Context::default();
    state.set_egui_ctx(ctx.clone());
    for _ in 0..500 {
        state.notify_ui();
        state.notify_chat_changed(user(1));
    }
    assert_eq!(
        state.repaint_requests(),
        1,
        "one request until the next frame starts"
    );
    // The next frame starts: a later change needs (and gets) a new request.
    state.set_egui_ctx(ctx);
    state.notify_ui();
    assert_eq!(state.repaint_requests(), 2);
}

#[test]
fn a_change_racing_a_frame_start_is_never_lost() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool as Flag;

    let state = Arc::new(LiveBridgeState::new());
    let ctx = egui::Context::default();
    state.set_egui_ctx(ctx.clone());
    let done = Arc::new(Flag::new(false));
    let producer = {
        let state = Arc::clone(&state);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            for _ in 0..20_000 {
                state.notify_chat_changed(user(1));
            }
            done.store(true, Ordering::Release);
        })
    };
    // The "UI": each frame starts, then reads the version it shows.
    let mut shown = 0;
    while !done.load(Ordering::Acquire) {
        state.set_egui_ctx(ctx.clone());
        shown = state.chat_version(user(1));
    }
    let _ = producer.join();
    // Either a repaint is still owed, or the last frame already showed the final state.
    let owed = state.repaint_pending.load(Ordering::Acquire);
    assert!(
        owed || shown == state.chat_version(user(1)),
        "a change after the last frame start was dropped without a pending repaint"
    );
}
