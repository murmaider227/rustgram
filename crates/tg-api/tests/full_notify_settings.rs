//! TGB-2980: the notification settings a `userFull` / `chatFull` / `channelFull` carries go
//! through `Session::set_peer_notify`, as dialog pages and `updateNotifySettings` do.
//!
//! Before, a profile refresh wrote the settings straight into the peer: the chat's mute state
//! (chat list icon, unread badge) kept its old value, and `NOTIFICATIONS` was announced on every
//! refresh, changed or not, which made the notification system rebuild its mute view each time.

use tg_api::convert::{apply_tl_chat_full, apply_tl_user_full};
use tg_data::{
    ChangeEvent, Channel, ChannelId, Chat, ChatId, PeerId, PeerUpdateFlags, Session, User, UserId,
};
use tg_tl::{enums as tl_enums, types as tl_types};

const FOREVER: i32 = i32::MAX;

fn notify(mute_until: Option<i32>) -> tl_enums::PeerNotifySettings {
    tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
        show_previews: None,
        silent: None,
        mute_until,
        ios_sound: None,
        android_sound: None,
        other_sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_ios_sound: None,
        stories_android_sound: None,
        stories_other_sound: None,
    })
}

fn user_full(id: i64, mute_until: Option<i32>) -> tl_enums::UserFull {
    tl_enums::UserFull::UserFull(tl_types::UserFull {
        blocked: false,
        phone_calls_available: false,
        phone_calls_private: false,
        can_pin_message: false,
        has_scheduled: false,
        video_calls_available: false,
        voice_messages_forbidden: false,
        translations_disabled: false,
        stories_pinned_available: false,
        blocked_my_stories_from: false,
        wallpaper_overridden: false,
        contact_require_premium: false,
        read_dates_private: false,
        sponsored_enabled: false,
        can_view_revenue: false,
        bot_can_manage_emoji_status: false,
        display_gifts_button: false,
        noforwards_my_enabled: false,
        noforwards_peer_enabled: false,
        unofficial_security_risk: false,
        id,
        about: None,
        settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
            report_spam: false,
            add_contact: false,
            block_contact: false,
            share_contact: false,
            need_contacts_exception: false,
            report_geo: false,
            autoarchived: false,
            invite_members: false,
            request_chat_broadcast: false,
            business_bot_paused: false,
            business_bot_can_reply: false,
            geo_distance: None,
            request_chat_title: None,
            request_chat_date: None,
            business_bot_id: None,
            business_bot_manage_url: None,
            charge_paid_message_stars: None,
            registration_month: None,
            phone_country: None,
            name_change_date: None,
            photo_change_date: None,
        }),
        personal_photo: None,
        profile_photo: None,
        fallback_photo: None,
        notify_settings: notify(mute_until),
        bot_info: None,
        pinned_msg_id: None,
        common_chats_count: 0,
        folder_id: None,
        ttl_period: None,
        theme: None,
        private_forward_name: None,
        bot_group_admin_rights: None,
        bot_broadcast_admin_rights: None,
        wallpaper: None,
        stories: None,
        business_work_hours: None,
        business_location: None,
        business_greeting_message: None,
        business_away_message: None,
        business_intro: None,
        birthday: None,
        personal_channel_id: None,
        personal_channel_message: None,
        stargifts_count: None,
        starref_program: None,
        bot_verification: None,
        send_paid_messages_stars: None,
        disallowed_gifts: None,
        stars_rating: None,
        stars_my_pending_rating: None,
        stars_my_pending_rating_date: None,
        main_tab: None,
        saved_music: None,
        note: None,
        bot_manager_id: None,
        wallet_card: None,
        share_theme: None,
    })
}

fn chat_full(id: i64, mute_until: Option<i32>) -> tl_enums::ChatFull {
    tl_enums::ChatFull::ChatFull(tl_types::ChatFull {
        can_set_username: false,
        has_scheduled: false,
        translations_disabled: false,
        has_welcome_messages: false,
        id,
        about: String::new(),
        participants: tl_enums::ChatParticipants::ChatParticipants(tl_types::ChatParticipants {
            chat_id: id,
            participants: Vec::new(),
            version: 1,
        }),
        chat_photo: None,
        notify_settings: notify(mute_until),
        exported_invite: None,
        bot_info: None,
        pinned_msg_id: None,
        folder_id: None,
        call: None,
        ttl_period: None,
        groupcall_default_join_as: None,
        theme_emoticon: None,
        requests_pending: None,
        recent_requesters: None,
        available_reactions: None,
        reactions_limit: None,
    })
}

fn channel_full(id: i64, mute_until: Option<i32>) -> tl_enums::ChatFull {
    tl_enums::ChatFull::ChannelFull(tl_types::ChannelFull {
        can_view_participants: false,
        can_set_username: false,
        can_set_stickers: false,
        hidden_prehistory: false,
        can_set_location: false,
        has_scheduled: false,
        can_view_stats: false,
        blocked: false,
        can_delete_channel: false,
        antispam: false,
        participants_hidden: false,
        translations_disabled: false,
        stories_pinned_available: false,
        view_forum_as_messages: false,
        restricted_sponsored: false,
        can_view_revenue: false,
        paid_media_allowed: false,
        can_view_stars_revenue: false,
        paid_reactions_available: false,
        stargifts_available: false,
        paid_messages_available: false,
        has_welcome_messages: false,
        id,
        about: String::new(),
        participants_count: None,
        admins_count: None,
        kicked_count: None,
        banned_count: None,
        online_count: None,
        read_inbox_max_id: 0,
        read_outbox_max_id: 0,
        unread_count: 0,
        chat_photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
        notify_settings: notify(mute_until),
        exported_invite: None,
        bot_info: Vec::new(),
        migrated_from_chat_id: None,
        migrated_from_max_id: None,
        pinned_msg_id: None,
        stickerset: None,
        available_min_id: None,
        folder_id: None,
        linked_chat_id: None,
        location: None,
        slowmode_seconds: None,
        slowmode_next_send_date: None,
        stats_dc: None,
        pts: 1,
        call: None,
        ttl_period: None,
        pending_suggestions: None,
        groupcall_default_join_as: None,
        theme_emoticon: None,
        requests_pending: None,
        recent_requesters: None,
        default_send_as: None,
        available_reactions: None,
        reactions_limit: None,
        stories: None,
        wallpaper: None,
        boosts_applied: None,
        boosts_unrestrict: None,
        emojiset: None,
        bot_verification: None,
        stargifts_count: None,
        send_paid_messages_stars: None,
        main_tab: None,
        guard_bot_id: None,
        share_theme: None,
    })
}

fn notifications_events(
    rx: &mut tokio::sync::broadcast::Receiver<ChangeEvent>,
    peer: PeerId,
) -> usize {
    let mut count = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            ChangeEvent::PeerUpdated { peer_id, flags }
                if peer_id == peer && flags.contains(PeerUpdateFlags::NOTIFICATIONS)
        ) {
            count += 1;
        }
    }
    count
}

/// Runs the same checks for one kind of full object: muting through a profile refresh mutes
/// the chat and is announced once; the same refresh again announces nothing; unmuting works
/// the same way.
fn check(
    session: &mut Session,
    peer: PeerId,
    apply: impl Fn(&mut Session, Option<i32>),
    what: &str,
) {
    session.ensure_history(peer);
    assert!(!session.peer_notify_known(peer), "{what}: unknown at first");
    let mut rx = session.subscribe();

    apply(session, Some(FOREVER));
    assert!(
        session.history(peer).is_some_and(|h| h.muted),
        "{what}: a mute that arrives with the full info mutes the chat"
    );
    assert!(
        session.peer_notify_known(peer),
        "{what}: settings are known"
    );
    assert_eq!(
        session
            .peer(peer)
            .and_then(|p| p.common().notify.mute_until),
        Some(FOREVER),
        "{what}"
    );
    assert!(
        notifications_events(&mut rx, peer) >= 1,
        "{what}: the change is announced"
    );

    apply(session, Some(FOREVER));
    assert_eq!(
        notifications_events(&mut rx, peer),
        0,
        "{what}: an unchanged refresh announces nothing"
    );

    apply(session, Some(0));
    assert!(
        session.history(peer).is_some_and(|h| !h.muted),
        "{what}: unmuting through the full info unmutes the chat"
    );
    assert!(notifications_events(&mut rx, peer) >= 1, "{what}");
}

#[test]
fn user_full_notify_settings_update_the_mute_state_and_announce_only_changes() {
    let mut session = Session::new(UserId(1));
    session.upsert_user(User::new(UserId(1), true), PeerUpdateFlags::empty());
    session.upsert_user(User::new(UserId(10), false), PeerUpdateFlags::empty());
    check(
        &mut session,
        PeerId::from_user(UserId(10)),
        |s, mute| {
            apply_tl_user_full(s, &user_full(10, mute));
        },
        "userFull",
    );
}

#[test]
fn chat_full_notify_settings_update_the_mute_state_and_announce_only_changes() {
    let mut session = Session::new(UserId(1));
    session.upsert_chat(Chat::new(ChatId(20)), PeerUpdateFlags::empty());
    check(
        &mut session,
        PeerId::from_chat(ChatId(20)),
        |s, mute| {
            apply_tl_chat_full(s, &chat_full(20, mute));
        },
        "chatFull",
    );
}

#[test]
fn channel_full_notify_settings_update_the_mute_state_and_announce_only_changes() {
    let mut session = Session::new(UserId(1));
    session.upsert_channel(Channel::new(ChannelId(30)), PeerUpdateFlags::empty());
    check(
        &mut session,
        PeerId::from_channel(ChannelId(30)),
        |s, mute| {
            apply_tl_chat_full(s, &channel_full(30, mute));
        },
        "channelFull",
    );
}
