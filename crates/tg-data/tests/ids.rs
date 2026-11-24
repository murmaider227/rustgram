use tg_data::*;

#[test]
fn peer_id_exact_tdesktop_bit_encoding() {
    // Cross-checked against Telegram/SourceFiles/data/data_peer_id.h:
    // UserId = ChatIdType<0>, ChatId = ChatIdType<1>, ChannelId = ChatIdType<2>, FakeChatId = ChatIdType<0x7F>
    // PeerId = bare | (Shift << 48), kChatTypeMask = 0xFFFFFFFFFFFFULL
    let bare = 0x0000_0001_2345_6789_u64;

    let user = PeerId::from_user(UserId(bare));
    assert_eq!(user.raw(), bare);
    assert!(user.is_user());
    assert!(!user.is_chat());
    assert!(!user.is_channel());
    assert!(!user.is_fake());
    assert_eq!(user.to_user(), Some(UserId(bare)));
    assert_eq!(user.to_chat(), None);
    assert_eq!(user.to_channel(), None);
    assert_eq!(user.bare(), bare);

    let chat = PeerId::from_chat(ChatId(bare));
    assert_eq!(chat.raw(), (1_u64 << 48) | bare);
    assert!(chat.is_chat());
    assert!(!chat.is_user());
    assert_eq!(chat.to_chat(), Some(ChatId(bare)));
    assert_eq!(chat.to_user(), None);
    assert_eq!(chat.bare(), bare);

    let channel = PeerId::from_channel(ChannelId(bare));
    assert_eq!(channel.raw(), (2_u64 << 48) | bare);
    assert!(channel.is_channel());
    assert_eq!(channel.to_channel(), Some(ChannelId(bare)));
    assert_eq!(channel.bare(), bare);

    let fake = PeerId::from_fake(FakeChatId(bare));
    assert_eq!(fake.raw(), (0x7F_u64 << 48) | bare);
    assert!(fake.is_fake());
    assert_eq!(fake.to_fake(), Some(FakeChatId(bare)));
    assert_eq!(fake.bare(), bare);

    // Zero peer ID matches C++ `peerIsUser(PeerId(0)) == true` and `is_zero()`.
    assert!(PeerId::ZERO.is_zero());
    assert!(PeerId::ZERO.is_user());
    assert_eq!(PeerId::ZERO.bare(), 0);
}

#[test]
fn peer_id_modern_and_legacy_serialization() {
    // Cross-checked against SerializePeerId / DeserializePeerId in data_peer_id.cpp.
    let cases = [
        PeerId::from_user(UserId(42)),
        PeerId::from_user(UserId(5_000_000_000)),
        PeerId::from_chat(ChatId(99_999)),
        PeerId::from_channel(ChannelId(1_234_567_890)),
        PeerId::from_fake(FakeChatId(777)),
    ];

    for peer in cases {
        let serialized = peer.serialize().expect("valid peer must serialize");
        // Modern format sets bit 55 (0x80 << 48).
        assert_ne!(serialized & (0x80_u64 << 48), 0);
        assert_eq!(PeerId::deserialize(serialized), peer);
    }

    // Legacy 32-bit format (bit 55 clear): type nibble in bits 32..36.
    assert_eq!(
        PeerId::deserialize(0x0_0000_002A),
        PeerId::from_user(UserId(42))
    );
    assert_eq!(
        PeerId::deserialize(0x1_0000_002A),
        PeerId::from_chat(ChatId(42))
    );
    assert_eq!(
        PeerId::deserialize(0x2_0000_002A),
        PeerId::from_channel(ChannelId(42))
    );
    assert_eq!(
        PeerId::deserialize(0xF_0000_002A),
        PeerId::from_fake(FakeChatId(42))
    );
    // Unknown legacy type nibble -> PeerId::ZERO.
    assert_eq!(PeerId::deserialize(0x5_0000_002A), PeerId::ZERO);
}

#[test]
fn peer_id_bot_api_conversions() {
    let user = PeerId::from_user(UserId(123_456_789));
    assert_eq!(user.to_bot_api_id(), Some(123_456_789));
    assert_eq!(PeerId::from_bot_api_id(123_456_789), Some(user));

    let chat = PeerId::from_chat(ChatId(987_654));
    assert_eq!(chat.to_bot_api_id(), Some(-987_654));
    assert_eq!(PeerId::from_bot_api_id(-987_654), Some(chat));

    let channel = PeerId::from_channel(ChannelId(1_234_567_890));
    assert_eq!(channel.to_bot_api_id(), Some(-1_001_234_567_890));
    assert_eq!(PeerId::from_bot_api_id(-1_001_234_567_890), Some(channel));

    assert_eq!(PeerId::ZERO.to_bot_api_id(), None);
    assert_eq!(PeerId::from_fake(FakeChatId(1)).to_bot_api_id(), None);
    assert_eq!(PeerId::from_bot_api_id(0), None);
    assert_eq!(PeerId::from_bot_api_id(-1_000_000_000_000), None);
}

#[test]
fn msg_id_exact_tdesktop_constants_and_ranges() {
    // Cross-checked against data_msg_id.h.
    assert_eq!(START_CLIENT_MSG_ID, MsgId(1 - (1_i64 << 58)));
    assert_eq!(CLIENT_MSG_IDS, 1_i64 << 31);
    assert_eq!(
        END_CLIENT_MSG_ID,
        MsgId(START_CLIENT_MSG_ID.0 + (1_i64 << 31))
    );
    assert_eq!(START_STORY_MSG_ID, MsgId(END_CLIENT_MSG_ID.0 + 1));
    assert_eq!(SERVER_MAX_STORY_ID, 1_i32 << 30);
    assert_eq!(
        END_STORY_MSG_ID,
        MsgId(START_STORY_MSG_ID.0 + (1_i64 << 30))
    );
    assert_eq!(SERVER_MAX_MSG_ID, MsgId(1_i64 << 56));
    assert_eq!(SCHEDULED_MAX_MSG_ID, MsgId((1_i64 << 56) + (1_i64 << 32)));
    assert_eq!(
        SHORTCUT_MAX_MSG_ID,
        MsgId(SCHEDULED_MAX_MSG_ID.0 + (1_i64 << 32))
    );
    assert_eq!(
        WELCOME_MAX_MSG_ID,
        MsgId(SHORTCUT_MAX_MSG_ID.0 + (1_i64 << 32))
    );
    assert_eq!(SHOW_AT_UNREAD_MSG_ID, MsgId(0));
    assert_eq!(SHOW_AT_THE_END_MSG_ID, MsgId(END_STORY_MSG_ID.0 + 1));
    assert_eq!(SWITCH_AT_TOP_MSG_ID, MsgId(END_STORY_MSG_ID.0 + 2));
    assert_eq!(SHOW_AND_START_BOT_MSG_ID, MsgId(END_STORY_MSG_ID.0 + 4));
    assert_eq!(
        SHOW_AND_MAYBE_START_BOT_MSG_ID,
        MsgId(END_STORY_MSG_ID.0 + 5)
    );
    assert_eq!(
        SHOW_FOR_CHOOSE_MESSAGES_MSG_ID,
        MsgId(END_STORY_MSG_ID.0 + 6)
    );

    // Server IDs.
    assert!(!MsgId(0).is_server());
    assert!(MsgId(1).is_server());
    assert!(MsgId(SERVER_MAX_MSG_ID.0 - 1).is_server());
    assert!(!SERVER_MAX_MSG_ID.is_server());

    // Client IDs.
    let c0 = MsgId::client_by_index(0).unwrap();
    assert_eq!(c0, START_CLIENT_MSG_ID);
    assert!(c0.is_client());
    assert_eq!(c0.client_index(), Some(0));
    let c42 = MsgId::client_by_index(42).unwrap();
    assert_eq!(c42.client_index(), Some(42));
    assert_eq!(MsgId::client_by_index(-1), None);

    // Story IDs.
    let s10 = MsgId::from_story_id(10).unwrap();
    assert!(s10.is_story());
    assert_eq!(s10.to_story_id(), Some(10));
    assert_eq!(MsgId::from_story_id(-1), None);

    // Scheduled / shortcut / welcome ranges (strictly > sentinel lower bounds,
    // matching `IsScheduledMsgId` / `IsShortcutMsgId` in tdesktop).
    assert!(!SERVER_MAX_MSG_ID.is_scheduled());
    assert!(MsgId(SERVER_MAX_MSG_ID.0 + 1).is_scheduled());
    assert!(!SCHEDULED_MAX_MSG_ID.is_scheduled());
    assert!(!SCHEDULED_MAX_MSG_ID.is_shortcut());
    assert!(MsgId(SCHEDULED_MAX_MSG_ID.0 + 1).is_shortcut());
    assert!(!SHORTCUT_MAX_MSG_ID.is_shortcut());
    assert!(!SHORTCUT_MAX_MSG_ID.is_welcome());
    assert!(MsgId(SHORTCUT_MAX_MSG_ID.0 + 1).is_welcome());
    assert!(!WELCOME_MAX_MSG_ID.is_welcome());
}

#[test]
fn color_index_and_message_group_id_and_draft_key() {
    assert_eq!(decide_color_index(PeerId::from_user(UserId(0))), 0);
    assert_eq!(decide_color_index(PeerId::from_user(UserId(10))), 3);
    assert_eq!(decide_color_index(PeerId::from_channel(ChannelId(10))), 3);
    assert_eq!(color_index_to_palette_index(0), 0);
    assert_eq!(color_index_to_palette_index(1), 7);
    assert_eq!(color_index_to_palette_index(6), 5);

    // Empty name uses constant 777 + (0xFE << 32) matching C++.
    assert_eq!(
        fake_peer_id_for_just_name(""),
        PeerId::from_user(UserId((0xFE_u64 << 32) + 777))
    );
    assert_ne!(
        fake_peer_id_for_just_name("Alice"),
        fake_peer_id_for_just_name("Bob")
    );

    let peer = PeerId::from_channel(ChannelId(500));
    let grp = MessageGroupId::from_raw(peer, 9999, true);
    assert!(grp.is_scheduled());
    assert_eq!(grp.peer_id(), peer);
    assert_eq!(grp.raw(), 9999);
    assert_eq!(grp.peer_and_scheduled_flag, peer.raw() | (1_u64 << 55));

    // DraftKey roundtrips.
    let local_main = DraftKey::local(MsgId::ZERO, PeerId::ZERO);
    assert!(local_main.is_local());
    assert!(!local_main.is_cloud());
    assert_eq!(local_main.serialize(), -1);

    let cloud_topic = DraftKey::cloud(MsgId(42), PeerId::ZERO);
    assert!(cloud_topic.is_cloud());
    assert_eq!(cloud_topic.topic_root_id(), MsgId(42));
    assert_eq!(
        DraftKey::from_serialized(cloud_topic.serialize()),
        cloud_topic
    );

    let mono_peer = PeerId::from_user(UserId(1234));
    let mono_local = DraftKey::local(MsgId::ZERO, mono_peer);
    assert!(mono_local.is_local());
    assert_eq!(mono_local.monoforum_peer_id(), mono_peer);
}
