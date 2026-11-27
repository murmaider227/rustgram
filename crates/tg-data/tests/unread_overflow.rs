//! Server unread counters near `i32::MAX` saturate instead of overflowing (TGB-3087, TGB-3089).

use tg_data::{MainList, UnreadState};

fn known(messages: i32) -> UnreadState {
    UnreadState {
        messages,
        chats: 1,
        known: true,
        ..Default::default()
    }
}

#[test]
fn unread_state_sum_saturates() {
    let total = known(i32::MAX) + known(1);
    assert_eq!(total.messages, i32::MAX);
    assert_eq!(total.chats, 2);
    let low = known(i32::MIN) - known(1);
    assert_eq!(low.messages, i32::MIN);
}

#[test]
fn unread_state_normal_values_unchanged() {
    let total = known(5) + known(7);
    assert_eq!(total.messages, 12);
    assert_eq!((total - known(7)).messages, 5);
}

#[test]
fn main_list_unread_total_saturates() {
    let mut list = MainList::new(0, 5);
    let zero = UnreadState {
        known: true,
        ..Default::default()
    };
    list.unread_state_changed(zero, known(i32::MAX));
    list.unread_state_changed(zero, known(1));
    assert_eq!(list.unread_state().messages, i32::MAX);
}

#[test]
fn archive_cloud_unread_saturates() {
    let mut list = MainList::new(1, 5);
    list.update_cloud_unread(1, i32::MAX, 1, i32::MAX);
    let state = list.unread_state();
    assert_eq!(state.messages, i32::MAX);
    assert_eq!(state.chats, i32::MAX);
    assert_eq!(state.messages_muted, i32::MAX);

    list.update_cloud_unread(3, 4, 1, 2);
    let state = list.unread_state();
    assert_eq!((state.messages, state.chats), (7, 3));
}
