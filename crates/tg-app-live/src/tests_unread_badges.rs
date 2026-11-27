//! The "All chats" tab badge counts unread chats like tdesktop's folder tabs (TGB-3160):
//! `chats + marks` of the main list without the archive, with the muted part reported so the
//! tab can grey or drop it (`ui/widgets/chat_filters_tabs_strip.cpp:238-252`).

use tg_app_core::dialogs::backend::FolderUnreadState;
use tg_app_core::notifications::settings::UnreadBadgeState;
use tg_app_core::stand_in::types::FilterId;
use tg_data::{ARCHIVE_FOLDER_ID, PeerId, PeerUpdateFlags, Session, User, UserId};

use crate::convert::{all_chats_dialog_folder, all_chats_folder_unread, app_unread_badge_state};

fn add_chat(session: &mut Session, id: u64, unread: i32, mark: bool, archived: bool) -> PeerId {
    let mut user = User::new(UserId(id), false);
    user.first_name = format!("Chat {id}");
    session.upsert_user(user, PeerUpdateFlags::NAME);
    let peer = PeerId::from_user(UserId(id));
    let history = session.ensure_history(peer);
    history.in_chat_list = true;
    history.folder = Some(archived.then_some(ARCHIVE_FOLDER_ID));
    history.unread_count = Some(unread);
    history.unread_mark = mark;
    session.sync_dialog_lists(peer);
    peer
}

/// Three unread chats holding 108 + 3 + 2 messages, one read chat, one archived unread chat.
fn walk_like_session() -> Session {
    let mut session = Session::new(UserId(1));
    add_chat(&mut session, 10, 108, false, false);
    add_chat(&mut session, 11, 3, false, false);
    add_chat(&mut session, 12, 2, false, false);
    add_chat(&mut session, 13, 0, false, false);
    add_chat(&mut session, 14, 40, false, true);
    session
}

#[test]
fn all_chats_tab_counts_unread_chats_not_messages() {
    let session = walk_like_session();
    // Before TGB-3160 the tab showed the app badge: every unread message of the main list.
    assert_eq!(session.total_unread_badge().unread_counter, 113);

    let folder = all_chats_dialog_folder(&session);
    assert_eq!(folder.id, FilterId::ALL);
    assert_eq!(
        folder.unread,
        FolderUnreadState {
            unread_chats: 3,
            muted_chats: 0,
        }
    );
    assert_eq!(folder.unread.badge_display(true), (3, false));
}

#[test]
fn all_chats_tab_counts_marked_chats_and_reports_muted_ones() {
    let mut session = walk_like_session();
    add_chat(&mut session, 15, 0, true, false);
    let muted = add_chat(&mut session, 16, 7, false, false);
    session.set_history_muted(muted, true);

    let unread = all_chats_folder_unread(&session);
    assert_eq!(
        unread,
        FolderUnreadState {
            unread_chats: 5,
            muted_chats: 1,
        }
    );
    // "Include muted chats" off drops the muted chat from the number.
    assert_eq!(unread.badge_display(false), (4, false));
}

#[test]
fn all_chats_tab_is_grey_when_only_muted_chats_are_unread() {
    let mut session = Session::new(UserId(1));
    let muted = add_chat(&mut session, 20, 12, false, false);
    session.set_history_muted(muted, true);
    let unread = all_chats_folder_unread(&session);
    assert_eq!(unread.badge_display(true), (1, true));
    assert_eq!(unread.badge_display(false), (0, false));
}

#[test]
fn app_badge_counts_archived_chats_as_muted() {
    // tdesktop's `Data::Session::unreadBadge` reads the main list, which holds the archive
    // folder entry; the archive's chats all count as muted.
    let session = walk_like_session();
    assert_eq!(
        app_unread_badge_state(&session),
        UnreadBadgeState {
            messages: 153,
            messages_muted: 40,
            chats: 4,
            chats_muted: 1,
            marks: 0,
            marks_muted: 0,
            mentions: 0,
        }
    );
}
