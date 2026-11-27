//! Opening a chat whose whole history is known needs no `messages.getHistory`, also when the
//! chat has no messages (TGB-3079: the first frame showed "loading" until the request returned,
//! so a streamed bot draft in an empty bot chat appeared one frame late, or later under load).

use tg_data::{ChannelId, MsgId as DataMsgId, MsgRange, PeerId as DataPeerId, SERVER_MAX_MSG_ID};
use tg_tl::functions as tl_fns;

use super::tests_reply_targets::{PEER_ID, build_live, tl_message, tl_user_peer};
use tg_app_core::history::backend::{HistoryBackend, Loadable};
use tg_app_core::stand_in::types::{MsgId, PeerId};

#[test]
fn a_complete_empty_chat_opens_without_a_history_request() {
    let user = DataPeerId::from_user(tg_data::UserId(PEER_ID as u64));
    let (rpc, live) = build_live(
        user,
        vec![tl_message(
            10,
            tl_user_peer(PEER_ID),
            PEER_ID,
            "hello",
            None,
        )],
    );
    let group = DataPeerId::from_channel(ChannelId(700));
    live.write_session(|s| {
        s.ensure_history(group).loaded_ranges.add_slice(
            std::iter::empty(),
            MsgRange::new(DataMsgId(0), SERVER_MAX_MSG_ID),
            Some(0),
        );
    });

    let first = live.try_load_slice_around(PeerId(group.0), MsgId::SHOW_AT_UNREAD, 50, 0);
    let Loadable::Ready(slice) = first else {
        panic!("a complete empty history must be ready on the first frame, got {first:?}");
    };
    assert!(slice.messages.is_empty());
    assert!(slice.loaded_at_bottom);
    assert!(rpc.calls::<tl_fns::messages::GetHistory>().is_empty());
}
