use std::collections::BTreeSet;

use tg_data::*;

#[test]
fn dialog_sort_keys_match_tdesktop_constants_and_ordering() {
    // Exact C++ constants from dialogs/dialogs_entry.cpp:
    // FixedOnTopDialogPos(index) = 0xFFFFFFFFFFFF000FULL - index
    // PinnedDialogPos(index)     = 0xFFFFFFFF000000FFULL - index
    // UnreadOnTopDialogPos(k)    = k | 0x8000000000000000ULL
    // DialogPosFromDate(d, s)    = (uint64(d) << 32) | s
    assert_eq!(
        fixed_on_top_dialog_pos(ARCHIVE_FIX_ON_TOP_INDEX),
        0xFFFF_FFFF_FFFF_000E
    );
    assert_eq!(
        fixed_on_top_dialog_pos(TOP_PROMOTION_FIX_ON_TOP_INDEX),
        0xFFFF_FFFF_FFFF_000D
    );
    assert_eq!(pinned_dialog_pos(1), 0xFFFF_FFFF_0000_00FE);
    assert_eq!(pinned_dialog_pos(2), 0xFFFF_FFFF_0000_00FD);
    assert_eq!(dialog_pos_from_date(0, 5), 0);
    assert_eq!(
        dialog_pos_from_date(1_700_000_000, 3),
        (1_700_000_000_u64 << 32) | 3
    );

    let k_archive = fixed_on_top_dialog_pos(ARCHIVE_FIX_ON_TOP_INDEX);
    let k_promo = fixed_on_top_dialog_pos(TOP_PROMOTION_FIX_ON_TOP_INDEX);
    let k_pin1 = pinned_dialog_pos(1);
    let k_pin2 = pinned_dialog_pos(2);
    let k_unread_old = unread_on_top_dialog_pos(dialog_pos_from_date(1_600_000_000, 1));
    let k_read_new = dialog_pos_from_date(1_700_000_000, 2);
    let k_read_old = dialog_pos_from_date(1_600_000_000, 1);

    assert!(k_archive > k_promo);
    assert!(k_promo > k_pin1);
    assert!(k_pin1 > k_pin2);
    assert!(k_pin2 > k_unread_old);
    assert!(k_unread_old > k_read_new);
    assert!(k_read_new > k_read_old);
    assert!(k_read_old > 0);
}

#[test]
fn chat_filter_matching_all_rules_and_edge_cases() {
    // Contact user.
    let mut contact = User::new(UserId(1), false);
    contact.contact_status = peer::ContactStatus::Contact;
    let contact_peer = Peer::User(Box::new(contact));
    let mut contact_hist = History::new(contact_peer.id());
    contact_hist.set_folder(None);
    contact_hist.unread_count = Some(3);

    // Non-contact user.
    let non_contact_peer = Peer::User(Box::new(User::new(UserId(2), false)));
    let mut non_contact_hist = History::new(non_contact_peer.id());
    non_contact_hist.set_folder(None);
    non_contact_hist.unread_count = Some(0);

    // Bot.
    let mut bot = User::new(UserId(3), false);
    bot.bot_info = Some(BotInfo::default());
    let bot_peer = Peer::User(Box::new(bot));
    let mut bot_hist = History::new(bot_peer.id());
    bot_hist.set_folder(None);
    bot_hist.unread_count = Some(0);

    // Basic group (muted with unread mention).
    let chat_peer = Peer::Chat(Box::new(Chat::new(ChatId(4))));
    let mut chat_hist = History::new(chat_peer.id());
    chat_hist.set_folder(None);
    chat_hist.muted = true;
    chat_hist.unread_count = Some(2);
    chat_hist.unread_mentions_count = 1;

    // Broadcast channel (archived).
    let mut ch = Channel::new(ChannelId(5));
    ch.set_flags(ChannelFlags::BROADCAST);
    let ch_peer = Peer::Channel(Box::new(ch));
    let mut ch_hist = History::new(ch_peer.id());
    ch_hist.set_folder(Some(ARCHIVE_FOLDER_ID));
    ch_hist.unread_count = Some(1);

    // Megagroup counts as Groups, not Channels.
    let mut mg = Channel::new(ChannelId(6));
    mg.set_flags(ChannelFlags::MEGAGROUP);
    let mg_peer = Peer::Channel(Box::new(mg));
    let mut mg_hist = History::new(mg_peer.id());
    mg_hist.set_folder(None);
    mg_hist.unread_count = Some(0);

    // Filter: Contacts + Groups + NoMuted + NoRead + NoArchived.
    let filter = ChatFilter::new(
        2,
        TextWithEntities::plain("Work"),
        "💼",
        Some(1),
        ChatFilterFlags::CONTACTS
            | ChatFilterFlags::GROUPS
            | ChatFilterFlags::NO_MUTED
            | ChatFilterFlags::NO_READ
            | ChatFilterFlags::NO_ARCHIVED,
        BTreeSet::from([bot_peer.id()]),
        vec![],
        BTreeSet::from([mg_peer.id()]),
    );

    // Unread unmuted contact matches.
    assert!(filter.contains(&contact_peer, &contact_hist, false));
    // Non-contact does not match.
    assert!(!filter.contains(&non_contact_peer, &non_contact_hist, false));
    // Bot matches because it is in `always` despite flags.
    assert!(filter.contains(&bot_peer, &bot_hist, false));
    // Muted group with unread mention in unarchived chat STILL matches NoMuted!
    assert!(filter.contains(&chat_peer, &chat_hist, false));
    // Remove mention -> muted group is now excluded by NoMuted.
    chat_hist.unread_mentions_count = 0;
    assert!(!filter.contains(&chat_peer, &chat_hist, false));
    // Archived channel excluded.
    assert!(!filter.contains(&ch_peer, &ch_hist, false));
    // `never` overrides even `always`.
    let filter_never_wins = ChatFilter::new(
        3,
        TextWithEntities::plain("Never"),
        "",
        None,
        ChatFilterFlags::GROUPS,
        BTreeSet::from([mg_peer.id()]),
        vec![],
        BTreeSet::from([mg_peer.id()]),
    );
    assert!(!filter_never_wins.contains(&mg_peer, &mg_hist, false));

    // `fake_unread_while_opened` keeps a read chat in `NoRead` unless `ignore_fake_unread = true`.
    let mut opened_contact_hist = contact_hist.clone();
    opened_contact_hist.unread_count = Some(0);
    opened_contact_hist.fake_unread_while_opened = true;
    assert!(filter.contains(&contact_peer, &opened_contact_hist, false));
    assert!(!filter.contains(&contact_peer, &opened_contact_hist, true));
}

#[test]
fn unread_counters_and_badges_for_unread_rules() {
    let state = UnreadState {
        messages: 10,
        messages_muted: 7,
        chats: 3,
        chats_muted: 2,
        marks: 1,
        marks_muted: 0,
        reactions: 1,
        reactions_muted: 1,
        mentions: 1,
        polls: 0,
        polls_muted: 0,
        known: true,
    };

    // Messages + include muted: 10 + 1 mark = 11, not all muted.
    let b1 = badges_for_unread(
        &state,
        CountInBadge::Messages,
        IncludeInBadge::All,
        BadgeSettings::default(),
    );
    assert_eq!(b1.unread_counter, 11);
    assert!(b1.unread);
    assert!(!b1.unread_muted);
    assert!(b1.mention);
    assert!(b1.reaction && b1.reaction_muted);

    // Messages + unmuted only: (10 - 7) + (1 - 0) = 4.
    let b2 = badges_for_unread(
        &state,
        CountInBadge::Messages,
        IncludeInBadge::Unmuted,
        BadgeSettings::default(),
    );
    assert_eq!(b2.unread_counter, 4);
    assert!(b2.unread);
    assert!(!b2.unread_muted);

    // Chats + unmuted only: (3 - 2) + (1 - 0) = 2.
    let b3 = badges_for_unread(
        &state,
        CountInBadge::Chats,
        IncludeInBadge::Unmuted,
        BadgeSettings::default(),
    );
    assert_eq!(b3.unread_counter, 2);

    // Single unread mark with 0 messages renders as a dot (`unread = true, unread_counter = 0`).
    let single_mark = UnreadState {
        marks: 1,
        known: true,
        ..Default::default()
    };
    let b_mark = badges_for_unread(
        &single_mark,
        CountInBadge::Messages,
        IncludeInBadge::All,
        BadgeSettings::default(),
    );
    assert!(b_mark.unread);
    assert_eq!(b_mark.unread_counter, 0);

    // UnmutedOrAll falls back to muted count when there are no unmuted unreads.
    let only_muted = UnreadState {
        messages: 5,
        messages_muted: 5,
        chats: 2,
        chats_muted: 2,
        known: true,
        ..Default::default()
    };
    let b_fallback = badges_for_unread(
        &only_muted,
        CountInBadge::Chats,
        IncludeInBadge::UnmutedOrAll,
        BadgeSettings::default(),
    );
    assert!(b_fallback.unread);
    assert!(b_fallback.unread_muted);
    assert_eq!(b_fallback.unread_counter, 2);
}

#[test]
fn sparse_ids_list_merging_and_queries() {
    let mut list = SparseIdsList::new();
    list.add_slice(
        [MsgId(10), MsgId(12), MsgId(15)],
        MsgRange::new(MsgId(10), MsgId(15)),
        Some(20),
    );
    list.add_slice(
        [MsgId(20), MsgId(25)],
        MsgRange::new(MsgId(20), MsgId(25)),
        None,
    );
    assert_eq!(list.slices().len(), 2);

    // Bridge the gap -> slices merge into one.
    list.add_slice(
        [MsgId(15), MsgId(18), MsgId(20)],
        MsgRange::new(MsgId(15), MsgId(20)),
        None,
    );
    assert_eq!(list.slices().len(), 1);
    assert_eq!(list.slices()[0].range, MsgRange::new(MsgId(10), MsgId(25)));

    // Add new message at bottom -> marks loaded_at_bottom.
    list.add_new(MsgId(25));
    list.add_new(MsgId(30));
    assert!(list.loaded_at_bottom());
    assert_eq!(list.count_after(MsgId(18), 100, |_| true), Some(3)); // 20, 25, 30

    let snap = list.snapshot(MsgId(18), 2, 2);
    assert_eq!(
        snap.message_ids,
        vec![MsgId(12), MsgId(15), MsgId(18), MsgId(20), MsgId(25)]
    );
    assert_eq!(snap.skipped_after, Some(1)); // MsgId(30)

    list.invalidate_bottom();
    assert!(!list.loaded_at_bottom());
}

#[tokio::test]
async fn session_registry_dialogs_filters_archive_and_broadcast() {
    let mut session = Session::new(UserId(1));
    let mut rx = session.subscribe();

    let mut u1 = User::new(UserId(10), false);
    u1.set_name("Alice", "Wonder", "Alice Wonder", "alice");
    u1.contact_status = peer::ContactStatus::Contact;
    session.upsert_user(u1, PeerUpdateFlags::NAME);

    let mut u2 = User::new(UserId(20), false);
    u2.set_name("Bob", "Builder", "Bob Builder", "bob");
    session.upsert_user(u2, PeerUpdateFlags::NAME);

    let p1 = PeerId::from_user(UserId(10));
    let p2 = PeerId::from_user(UserId(20));

    session.set_history_folder(p1, None);
    session.set_history_folder(p2, None);

    session.insert_message(Message::new_text(
        p1,
        MsgId(1),
        Some(p1),
        1_000,
        TextWithEntities::plain("Hello"),
    ));
    session.insert_message(Message::new_text(
        p2,
        MsgId(2),
        Some(p2),
        2_000,
        TextWithEntities::plain("World"),
    ));
    session.set_history_unread_count(p1, 2);
    session.set_history_unread_count(p2, 5);
    session.set_history_muted(p2, true);

    // Newer p2 sorts ahead of older p1 before pinning.
    assert_eq!(
        session.main_dialogs(),
        &[DialogKey::History(p2), DialogKey::History(p1)]
    );

    // Pin p1 -> p1 moves above p2.
    session.set_pinned(DialogKey::History(p1), 0, true);
    assert_eq!(
        session.main_dialogs(),
        &[DialogKey::History(p1), DialogKey::History(p2)]
    );

    // Unread badge: with include_muted_counter = false, only p1's 2 messages count.
    session.set_badge_settings(BadgeSettings {
        count_unread_messages: true,
        include_muted_counter: false,
    });
    let badge = session.total_unread_badge();
    assert_eq!(badge.unread_counter, 2);

    // Add a Contacts filter.
    let contacts_filter = ChatFilter::new(
        10,
        TextWithEntities::plain("Contacts"),
        "👤",
        None,
        ChatFilterFlags::CONTACTS,
        BTreeSet::new(),
        vec![],
        BTreeSet::new(),
    );
    session.set_filter(contacts_filter);
    assert_eq!(
        session.filter_dialogs(10),
        Some(&[DialogKey::History(p1)][..])
    );
    assert_eq!(session.filter_unread_badge(10).unwrap().unread_counter, 2);

    // Archive p2 -> Archive folder appears fixed at the very top of main_dialogs!
    session.set_history_folder(p2, Some(ARCHIVE_FOLDER_ID));
    assert_eq!(
        session.main_dialogs(),
        &[DialogKey::Folder(ARCHIVE_FOLDER_ID), DialogKey::History(p1)]
    );
    assert_eq!(session.archive_dialogs(), &[DialogKey::History(p2)]);
    assert_eq!(session.archive().unread_state().messages, 5);
    assert_eq!(session.archive().unread_state().messages_muted, 5);

    // Broadcast channel received our events.
    let first_event = rx.recv().await.expect("should receive broadcast event");
    assert_eq!(
        first_event,
        ChangeEvent::PeerUpdated {
            peer_id: p1,
            flags: PeerUpdateFlags::NAME,
        }
    );

    // Non-channel message lookup & deletion.
    assert!(session.non_channel_message(MsgId(1)).is_some());
    assert_eq!(session.delete_non_channel_messages(&[MsgId(1)]), 1);
    assert!(session.non_channel_message(MsgId(1)).is_none());

    // Prefix search in main dialogs list.
    assert_eq!(
        session.main_list().indexed().filter_by_query("ali won"),
        vec![DialogKey::History(p1)]
    );
}

#[test]
fn pinned_list_eviction_reorder_and_archive_cloud_unread() {
    let mut pinned = PinnedList::new(0, 2);
    let k1 = DialogKey::History(PeerId::from_user(UserId(1)));
    let k2 = DialogKey::History(PeerId::from_user(UserId(2)));
    let k3 = DialogKey::History(PeerId::from_user(UserId(3)));

    assert!(pinned.set_pinned(k1, true).is_empty());
    assert!(pinned.set_pinned(k2, true).is_empty());
    assert_eq!(pinned.order(), &[k2, k1]);
    assert_eq!(pinned.pinned_index(k2), 1);
    assert_eq!(pinned.pinned_index(k1), 2);

    // Pinning k3 evicts tail (k1) and places k3 at index 1.
    let evicted = pinned.set_pinned(k3, true);
    assert_eq!(evicted, vec![k1]);
    assert_eq!(pinned.order(), &[k3, k2]);

    // Reorder swaps positions.
    assert!(pinned.reorder(k3, k2));
    assert_eq!(pinned.order(), &[k2, k3]);

    // Archive cloud unread fallback before loaded=true.
    let mut folder = Folder::new(ARCHIVE_FOLDER_ID, 100);
    folder.chats_list.update_cloud_unread(4, 2, 1, 1);
    assert!(folder.chats_list.cloud_unread_known());
    let cloud_state = folder.unread_state();
    assert_eq!(cloud_state.messages, 6);
    assert_eq!(cloud_state.messages_muted, 6);
    assert_eq!(cloud_state.chats, 2);
    assert_eq!(cloud_state.chats_muted, 2);

    // Once fully loaded, local unread state takes over.
    folder.chats_list.set_loaded(true);
    assert_eq!(folder.unread_state().messages, 0);
}
