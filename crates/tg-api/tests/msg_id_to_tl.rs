//! Message IDs sent to the API must be regular server IDs (TGB-3060).
//!
//! `MsgId` is 64-bit; the TL `int` is 32-bit. Local (client), scheduled and story IDs used to be
//! narrowed with `as i32`, which keeps the low 32 bits: client index `k` becomes server ID `k + 1`
//! and scheduled ID `SERVER_MAX + n` becomes `n`, so the request named an unrelated real message.

use tg_api::channels::{build_read_channel_history, build_read_common_message_contents};
use tg_data::id::{MsgId, SERVER_MAX_MSG_ID};
use tg_tl::enums as tl_enums;

fn client_id(index: i32) -> MsgId {
    MsgId::client_by_index(index).unwrap_or(MsgId::ZERO)
}

#[test]
fn local_and_scheduled_ids_never_alias_server_ids() {
    let scheduled = MsgId(SERVER_MAX_MSG_ID.0 + 5);
    assert_eq!(
        client_id(6).0 as i32,
        7,
        "the old cast aliased client 6 to server 7"
    );

    let req = build_read_common_message_contents(&[MsgId(7), client_id(6), scheduled]);
    assert_eq!(req.id, vec![7]);

    let req = build_read_channel_history(tl_enums::InputChannel::Empty, client_id(41));
    assert_eq!(req.max_id, 0);
    let req = build_read_channel_history(tl_enums::InputChannel::Empty, MsgId(42));
    assert_eq!(req.max_id, 42);
}

#[test]
fn server_i32_accepts_only_regular_server_ids_in_range() {
    assert_eq!(MsgId(1).server_i32(), Some(1));
    assert_eq!(MsgId(i64::from(i32::MAX)).server_i32(), Some(i32::MAX));
    assert_eq!(MsgId(i64::from(i32::MAX) + 1).server_i32(), None);
    assert_eq!(MsgId::ZERO.server_i32(), None);
    assert_eq!(MsgId(-5).server_i32(), None);
    assert_eq!(client_id(0).server_i32(), None);
    assert_eq!(MsgId(SERVER_MAX_MSG_ID.0 + 1).server_i32(), None);
}
