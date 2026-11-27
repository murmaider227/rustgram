//! Requests whose failure tdesktop ignores (`.send()` without `.fail()`) go out without the UI
//! waiting, and a failing server leaves the local change in place, as in tdesktop (TGB-3071).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use tg_data::{ChatFilter, ChatFilterFlags, PeerId as DataPeerId, UserId as DataUserId};
use tg_tl::functions as tl_fns;

use super::tests_reply_targets::{FakeRpcClient, PEER_ID, build_live, tl_message, tl_user_peer};
use crate::LiveSession;
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::settings::backend::SettingsBackend;
use tg_app_core::stand_in::types::PeerId;

/// Simulated round trip of the fake server, which then fails every request.
const ROUND_TRIP: Duration = Duration::from_millis(400);

fn data_peer() -> DataPeerId {
    DataPeerId::from_user(DataUserId(PEER_ID as u64))
}

fn peer() -> PeerId {
    PeerId(data_peer().0)
}

/// A session whose server answers every request with an error after [`ROUND_TRIP`].
fn slow_failing_server() -> (FakeRpcClient, LiveSession) {
    let (rpc, live) = build_live(
        data_peer(),
        vec![tl_message(
            10,
            tl_user_peer(PEER_ID),
            PEER_ID,
            "hello",
            None,
        )],
    );
    rpc.set_handler(|_, _| {
        std::thread::sleep(ROUND_TRIP);
        None
    });
    (rpc, live)
}

fn assert_returned_at_once(started: Instant, what: &str) {
    let waited = started.elapsed();
    eprintln!("{what} returned after {waited:?}");
    assert!(waited < ROUND_TRIP / 2, "{what}: the UI waited {waited:?}");
}

#[test]
fn mark_dialog_read_clears_the_unread_mark_without_waiting() {
    let (rpc, mut live) = slow_failing_server();
    live.write_session(|s| s.set_history_unread_mark(data_peer(), true));

    let started = Instant::now();
    live.mark_dialog_read(peer());
    assert_returned_at_once(started, "mark_dialog_read");

    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    assert_eq!(rpc.calls::<tl_fns::messages::MarkDialogUnread>().len(), 1);
    let still_marked = live.read_session(|s| s.history(data_peer()).is_some_and(|h| h.unread_mark));
    assert!(
        !still_marked,
        "tdesktop keeps the local change when the request fails"
    );
}

#[test]
fn clear_payment_info_does_not_wait_for_the_server() {
    let (rpc, mut live) = slow_failing_server();

    let started = Instant::now();
    live.clear_payment_info(true, false);
    assert_returned_at_once(started, "clear_payment_info");

    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    let calls = rpc.calls::<tl_fns::payments::ClearSavedInfo>();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].info);
    assert!(!calls[0].credentials);
}

#[test]
fn removing_a_chat_from_folders_sends_each_folder_without_waiting() {
    let (rpc, mut live) = slow_failing_server();
    live.write_session(|s| {
        for id in [3, 4] {
            s.set_filter(ChatFilter::new(
                id,
                tg_data::TextWithEntities {
                    text: format!("Folder {id}"),
                    entities: Vec::new(),
                },
                "",
                None,
                // A rule besides the chat itself keeps the folder valid without it.
                ChatFilterFlags::CONTACTS,
                BTreeSet::from([data_peer()]),
                Vec::new(),
                BTreeSet::new(),
            ));
        }
    });

    let started = Instant::now();
    live.remove_peer_from_all_folders(peer());
    assert_returned_at_once(started, "remove_peer_from_all_folders");

    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    let mut sent: Vec<i32> = rpc
        .calls::<tl_fns::messages::UpdateDialogFilter>()
        .iter()
        .map(|c| c.id)
        .collect();
    sent.sort_unstable();
    assert_eq!(sent, vec![3, 4]);
    let left_in_folders = live.read_session(|s| {
        [3, 4]
            .iter()
            .filter(|id| {
                s.filters()
                    .get(**id)
                    .is_some_and(|f| f.always.contains(&data_peer()))
            })
            .count()
    });
    assert_eq!(left_in_folders, 0, "the local folders keep the removal");
}
