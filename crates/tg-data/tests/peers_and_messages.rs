use tg_data::*;

#[test]
fn api_bitflags_match_schema_positions() {
    // Exact bit positions from schema/api.tl `chatAdminRights#5fb224d5`.
    assert_eq!(ChatAdminRights::CHANGE_INFO.bits(), 1 << 0);
    assert_eq!(ChatAdminRights::POST_MESSAGES.bits(), 1 << 1);
    assert_eq!(ChatAdminRights::EDIT_MESSAGES.bits(), 1 << 2);
    assert_eq!(ChatAdminRights::DELETE_MESSAGES.bits(), 1 << 3);
    assert_eq!(ChatAdminRights::BAN_USERS.bits(), 1 << 4);
    assert_eq!(ChatAdminRights::INVITE_USERS.bits(), 1 << 5);
    assert_eq!(ChatAdminRights::PIN_MESSAGES.bits(), 1 << 7);
    assert_eq!(ChatAdminRights::ADD_ADMINS.bits(), 1 << 9);
    assert_eq!(ChatAdminRights::ANONYMOUS.bits(), 1 << 10);
    assert_eq!(ChatAdminRights::MANAGE_CALL.bits(), 1 << 11);
    assert_eq!(ChatAdminRights::OTHER.bits(), 1 << 12);
    assert_eq!(ChatAdminRights::MANAGE_TOPICS.bits(), 1 << 13);
    assert_eq!(ChatAdminRights::POST_STORIES.bits(), 1 << 14);
    assert_eq!(ChatAdminRights::EDIT_STORIES.bits(), 1 << 15);
    assert_eq!(ChatAdminRights::DELETE_STORIES.bits(), 1 << 16);
    assert_eq!(ChatAdminRights::MANAGE_DIRECT_MESSAGES.bits(), 1 << 17);
    assert_eq!(ChatAdminRights::MANAGE_RANKS.bits(), 1 << 18);
    assert_eq!(ChatAdminRights::MANAGE_LINKED_PEERS.bits(), 1 << 19);
    assert_eq!(ChatAdminRights::MANAGE_WELCOME_MESSAGES.bits(), 1 << 20);

    // Exact bit positions from schema/api.tl `chatBannedRights#9f120418`.
    assert_eq!(ChatBannedRights::VIEW_MESSAGES.bits(), 1 << 0);
    assert_eq!(ChatBannedRights::SEND_MESSAGES.bits(), 1 << 1);
    assert_eq!(ChatBannedRights::SEND_MEDIA.bits(), 1 << 2);
    assert_eq!(ChatBannedRights::SEND_STICKERS.bits(), 1 << 3);
    assert_eq!(ChatBannedRights::SEND_GIFS.bits(), 1 << 4);
    assert_eq!(ChatBannedRights::SEND_GAMES.bits(), 1 << 5);
    assert_eq!(ChatBannedRights::SEND_INLINE.bits(), 1 << 6);
    assert_eq!(ChatBannedRights::EMBED_LINKS.bits(), 1 << 7);
    assert_eq!(ChatBannedRights::SEND_POLLS.bits(), 1 << 8);
    assert_eq!(ChatBannedRights::CHANGE_INFO.bits(), 1 << 10);
    assert_eq!(ChatBannedRights::INVITE_USERS.bits(), 1 << 15);
    assert_eq!(ChatBannedRights::PIN_MESSAGES.bits(), 1 << 17);
    assert_eq!(ChatBannedRights::MANAGE_TOPICS.bits(), 1 << 18);
    assert_eq!(ChatBannedRights::SEND_PHOTOS.bits(), 1 << 19);
    assert_eq!(ChatBannedRights::SEND_VIDEOS.bits(), 1 << 20);
    assert_eq!(ChatBannedRights::SEND_ROUNDVIDEOS.bits(), 1 << 21);
    assert_eq!(ChatBannedRights::SEND_AUDIOS.bits(), 1 << 22);
    assert_eq!(ChatBannedRights::SEND_VOICES.bits(), 1 << 23);
    assert_eq!(ChatBannedRights::SEND_DOCS.bits(), 1 << 24);
    assert_eq!(ChatBannedRights::SEND_PLAIN.bits(), 1 << 25);
    assert_eq!(ChatBannedRights::EDIT_RANK.bits(), 1 << 26);
    assert_eq!(ChatBannedRights::SEND_REACTIONS.bits(), 1 << 27);
    assert_eq!(ChatBannedRights::MANAGE_LINKED_PEERS.bits(), 1 << 28);
}

#[test]
fn peer_permissions_can_send_and_restrictions() {
    let ctx = SendCheckContext::default();
    let premium_ctx = SendCheckContext {
        account_premium: true,
        ..Default::default()
    };
    let frozen_ctx = SendCheckContext {
        account_frozen: true,
        ..Default::default()
    };

    // Regular user.
    let mut alice = User::new(UserId(100), false);
    alice.set_name("Alice", "Smith", "Alice Smith", "alice");
    let alice_peer = Peer::User(Box::new(alice.clone()));
    assert!(alice_peer.can_write(ctx));
    assert!(!alice_peer.can_write(frozen_ctx));

    // @spambot remains writable even when account is frozen.
    let mut spambot = User::new(UserId(178_220_800), false);
    spambot.set_name("Spam Info Bot", "", "", "SpamBot");
    let spambot_peer = Peer::User(Box::new(spambot));
    assert!(spambot_peer.can_write(frozen_ctx));

    // User requiring premium to write + voice forbidden.
    let mut vip = User::new(UserId(101), false);
    vip.flags = UserFlags::REQUIRES_PREMIUM_TO_WRITE | UserFlags::VOICE_MESSAGES_FORBIDDEN;
    let vip_peer = Peer::User(Box::new(vip));
    assert!(!vip_peer.can_write(ctx));
    assert_eq!(
        vip_peer.am_restricted(ChatBannedRights::SEND_PLAIN, ctx),
        RestrictionCheckResult::Explicit
    );
    assert!(vip_peer.can_write(premium_ctx));
    assert!(!vip_peer.can_send(ChatBannedRights::SEND_VOICES, true, premium_ctx));
    assert!(vip_peer.can_send_texts(true, premium_ctx));

    // Basic chat restrictions & admin override.
    let mut chat = Chat::new(ChatId(200));
    chat.default_restrictions = ChatBannedRights::ALL_SEND;
    let chat_peer = Peer::Chat(Box::new(chat.clone()));
    assert!(!chat_peer.can_write(ctx));
    assert_eq!(
        chat_peer.am_restricted(ChatBannedRights::SEND_PLAIN, ctx),
        RestrictionCheckResult::WithEveryone
    );

    let mut admin_chat = chat.clone();
    admin_chat.admin_rights = ChatAdminRights::DELETE_MESSAGES;
    let admin_chat_peer = Peer::Chat(Box::new(admin_chat));
    assert!(admin_chat_peer.can_write(ctx));

    // Broadcast channel: non-admin cannot write, admin with POST_MESSAGES can.
    let mut broadcast = Channel::new(ChannelId(300));
    broadcast.set_flags(ChannelFlags::BROADCAST);
    let broadcast_peer = Peer::Channel(Box::new(broadcast.clone()));
    assert!(!broadcast_peer.can_write(ctx));
    broadcast.admin_rights = ChatAdminRights::POST_MESSAGES;
    let broadcast_admin_peer = Peer::Channel(Box::new(broadcast));
    assert!(broadcast_admin_peer.can_write(ctx));

    // Megagroup with default restrictions lifted by boosts.
    let mut mg = Channel::new(ChannelId(301));
    mg.set_flags(ChannelFlags::MEGAGROUP | ChannelFlags::USERNAME);
    mg.default_restrictions = ChatBannedRights::ALL_SEND;
    assert!(!Peer::Channel(Box::new(mg.clone())).can_write(ctx));
    // Public megagroup restricts PIN_MESSAGES and CHANGE_INFO implicitly.
    assert_eq!(
        Peer::Channel(Box::new(mg.clone())).am_restricted(ChatBannedRights::PIN_MESSAGES, ctx),
        RestrictionCheckResult::WithEveryone
    );
    mg.ensure_mg_info().boosts_unrestrict = 2;
    mg.ensure_mg_info().boosts_applied = 2;
    assert!(Peer::Channel(Box::new(mg.clone())).can_write(ctx));

    // Personal restriction still blocks even when boosts lift default restrictions.
    mg.restrictions = ChatRestrictionsInfo::new(ChatBannedRights::ALL_SEND, RESTRICT_UNTIL_FOREVER);
    let mg_restricted_peer = Peer::Channel(Box::new(mg.clone()));
    assert!(!mg_restricted_peer.can_write(ctx));
    assert_eq!(
        mg_restricted_peer.am_restricted(ChatBannedRights::SEND_PLAIN, ctx),
        RestrictionCheckResult::Explicit
    );

    // Forum blocks sending at root when `forbid_in_forums = true`, allows when `false`.
    let mut forum = Channel::new(ChannelId(302));
    forum.set_flags(ChannelFlags::MEGAGROUP | ChannelFlags::FORUM);
    let forum_peer = Peer::Channel(Box::new(forum));
    assert!(!forum_peer.can_send_anything(true, ctx));
    assert!(forum_peer.can_send_anything(false, ctx));
}

#[test]
fn lastseen_status_serialization_buckets_and_text() {
    let now: TimeId = 1_700_000_000;

    // Buckets & serialization roundtrip.
    for status in [
        LastseenStatus::long_ago(false),
        LastseenStatus::recently(false),
        LastseenStatus::recently(true),
        LastseenStatus::within_week(false),
        LastseenStatus::within_month(true),
        LastseenStatus::online_till_status(now + 120, false, false),
        LastseenStatus::online_till_status(now + 30, true, true),
    ] {
        let raw = status.serialize();
        assert_eq!(LastseenStatus::from_serialized(raw), status);
    }

    // Legacy deserialization.
    assert!(LastseenStatus::from_legacy(-2).is_recently());
    assert!(LastseenStatus::from_legacy(-3).is_within_week());
    assert!(LastseenStatus::from_legacy(-4).is_within_month());
    assert!(LastseenStatus::from_legacy(0).is_long_ago());
    assert_eq!(LastseenStatus::from_legacy(now).online_till(), now);
    assert!(LastseenStatus::from_legacy(-now).is_local_online_value());

    // Status text classification.
    assert_eq!(
        OnlineStatusText::classify(
            LastseenStatus::online_till_status(now + 60, false, false),
            now,
            0
        ),
        OnlineStatusText::Online
    );
    assert_eq!(
        OnlineStatusText::classify(
            LastseenStatus::online_till_status(now - 30, false, false),
            now,
            0
        ),
        OnlineStatusText::JustNow
    );
    assert_eq!(
        OnlineStatusText::classify(
            LastseenStatus::online_till_status(now - 300, false, false),
            now,
            0
        ),
        OnlineStatusText::MinutesAgo(5)
    );
    assert_eq!(
        OnlineStatusText::classify(
            LastseenStatus::online_till_status(now - 7200, false, false),
            now,
            0
        ),
        OnlineStatusText::HoursAgo(2)
    );
    assert_eq!(
        OnlineStatusText::classify(LastseenStatus::recently(false), now, 0),
        OnlineStatusText::Recently
    );
    assert_eq!(
        OnlineStatusText::classify(LastseenStatus::within_week(false), now, 0),
        OnlineStatusText::WithinWeek
    );
    assert_eq!(
        OnlineStatusText::classify(LastseenStatus::within_month(false), now, 0),
        OnlineStatusText::WithinMonth
    );
    assert_eq!(
        OnlineStatusText::classify(LastseenStatus::long_ago(false), now, 0),
        OnlineStatusText::LongAgo
    );

    // Pseudo-online via made_action preserved across UserStatusInput::Recently.
    let mut user = User::new(UserId(50), false);
    user.lastseen = LastseenStatus::recently(false);
    assert!(user.made_action(now));
    assert!(user.is_online(now + 10));
    assert!(user.lastseen.is_local_online_value());
    let after_api =
        LastseenStatus::from_api(UserStatusInput::Recently { by_me: true }, user.lastseen);
    assert!(after_api.is_local_online_value());
    assert!(after_api.is_hidden_by_me());
    assert_eq!(after_api.online_till(), now + SET_ONLINE_AFTER_ACTIVITY);
    // Once expired, local pseudo-online renders as "last seen recently".
    assert_eq!(
        OnlineStatusText::classify(after_api, now + 60, 0),
        OnlineStatusText::Recently
    );
}

#[test]
fn slowmode_usernames_migration_and_utf16_entities() {
    // UsernamesInfo with collectible + editable usernames.
    let mut info = UsernamesInfo::default();
    info.set_usernames(&[
        Username {
            username: "collectible".into(),
            active: true,
            editable: false,
        },
        Username {
            username: "inactive".into(),
            active: false,
            editable: false,
        },
        Username {
            username: "editable_name".into(),
            active: true,
            editable: true,
        },
    ]);
    assert_eq!(info.username(), "collectible");
    assert_eq!(info.editable_username(), "editable_name");
    assert!(info.is_editable("editable_name"));
    assert!(!info.is_editable("collectible"));
    assert_eq!(info.usernames(), &["collectible", "editable_name"]);

    // Slowmode countdown.
    let mut ch = Channel::new(ChannelId(700));
    ch.set_flags(ChannelFlags::MEGAGROUP);
    ch.set_slowmode_seconds(30);
    assert!(ch.slowmode_applied());
    ch.grow_slowmode_last_message(1_000);
    assert_eq!(ch.slowmode_seconds_left(1_010), 20);
    assert_eq!(ch.slowmode_seconds_left(1_050), 0);
    // Admin is exempt from slowmode.
    ch.admin_rights = ChatAdminRights::DELETE_MESSAGES;
    assert!(!ch.slowmode_applied());

    // Migration basic chat -> megagroup.
    let mut session = Session::new(UserId(1));
    let mut mg = Channel::new(ChannelId(900));
    mg.set_flags(ChannelFlags::MEGAGROUP);
    session.upsert_channel(mg, PeerUpdateFlags::NAME);
    session.apply_migration(ChatId(800), ChannelId(900));
    assert_eq!(
        session.migrate_to_or_self(PeerId::from_chat(ChatId(800))),
        PeerId::from_channel(ChannelId(900))
    );
    assert!(session.chat(ChatId(800)).unwrap().is_deactivated());
    assert_eq!(
        session.channel(ChannelId(900)).unwrap().migrated_from(),
        Some(ChatId(800))
    );

    // UTF-16 entity offsets with non-BMP emoji (2 UTF-16 code units).
    let text = "Hi 😀 @alice!";
    // 'H'(0) 'i'(1) ' '(2) '😀'(3..5) ' '(5) '@'(6) 'a'(7) 'l'(8) 'i'(9) 'c'(10) 'e'(11) '!'(12)
    let entity = TextEntity::new(6, 6, EntityKind::Mention);
    let tw = TextWithEntities::new(text, vec![entity.clone()]);
    assert_eq!(tw.utf16_len(), 13);
    assert!(entity.valid_for_text(tw.utf16_len()));
    assert_eq!(entity.slice_text(&tw.text).as_deref(), Some("@alice"));
}
