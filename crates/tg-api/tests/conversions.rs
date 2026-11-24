//! Comprehensive tests for TL (`tg-tl`) <-> domain (`tg-data`) conversions:
//! peers, `min` merge rules, `access_hash` bookkeeping, InputPeer/InputUser/InputChannel,
//! Photo/Document/WebPage, Message/MessageService/MessageEmpty, all 20 MessageMedia variants,
//! MessageAction variants, all 25 MessageEntity constructors + round-trip, Dialog/Folder/Filter/Draft.

mod common;

use common::{sample_tl_channel, sample_tl_dialog, sample_tl_message, sample_tl_user};
use tg_api::convert::{
    apply_tl_chat, apply_tl_chat_full, apply_tl_dialog, apply_tl_message, apply_tl_user,
    apply_tl_user_full, dialog_filter_from_tl, document_from_tl, draft_from_tl,
    draft_to_save_request, entities_from_tl, entities_to_tl, input_channel_for,
    input_dialog_peer_for, input_peer_for, input_reply_to_from_reply_info, input_user_for,
    message_action_from_tl, message_from_tl, message_media_from_tl, peer_id_from_tl, photo_from_tl,
    typing_action_from_tl, webpage_from_tl,
};
use tg_data::{
    ARCHIVE_FOLDER_ID, ChannelFlags, ChannelId, ChatFilterFlags, ChatId, ContactStatus, DialogKey,
    DocumentId, DocumentKind, Draft, EntityKind, FullMsgId, LoadedStatus, MessageFlags,
    MessageMedia, MsgId, PeerId, PhotoId, ReactionId, ReplyInfo, ServiceAction, Session,
    TextEntity, TextWithEntities, TypingAction, UserFlags, UserId,
};
use tg_tl::{enums as tl_enums, types as tl_types};

fn empty_notify() -> tl_enums::PeerNotifySettings {
    tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
        show_previews: None,
        silent: None,
        mute_until: None,
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

fn sample_photo(id: i64) -> tl_enums::Photo {
    tl_enums::Photo::Photo(tl_types::Photo {
        has_stickers: true,
        id,
        access_hash: 4242,
        file_reference: vec![1, 2, 3],
        date: 1_700_000_000,
        sizes: vec![
            tl_enums::PhotoSize::PhotoStrippedSize(tl_types::PhotoStrippedSize {
                r#type: "i".into(),
                bytes: vec![0x01, 0x20, 0x20, 0xAB],
            }),
            tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "m".into(),
                w: 320,
                h: 240,
                size: 12_000,
            }),
            tl_enums::PhotoSize::Progressive(tl_types::PhotoSizeProgressive {
                r#type: "x".into(),
                w: 1280,
                h: 960,
                sizes: vec![5_000, 15_000, 48_000],
            }),
        ],
        video_sizes: None,
        dc_id: 2,
    })
}

fn sample_document(
    id: i64,
    mime: &str,
    attrs: Vec<tl_enums::DocumentAttribute>,
) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: 9876,
        file_reference: vec![9, 8],
        date: 1_700_000_100,
        mime_type: mime.to_owned(),
        size: 1_048_576,
        thumbs: Some(vec![
            tl_enums::PhotoSize::PhotoStrippedSize(tl_types::PhotoStrippedSize {
                r#type: "i".into(),
                bytes: vec![0x01, 0x10, 0x10],
            }),
            tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "m".into(),
                w: 90,
                h: 90,
                size: 1500,
            }),
        ]),
        video_thumbs: None,
        dc_id: 2,
        attributes: attrs,
    })
}

#[test]
fn peer_conversions_min_merge_migration_and_access_hash_bookkeeping() {
    let mut session = Session::new(UserId(1));

    // Full user load populates access_hash, phone, status, contact.
    let mut u = sample_tl_user(10, 0x1122_3344, "Alpha", false);
    u.contact = true;
    u.premium = true;
    u.status = Some(tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
        expires: 1_700_001_000,
    }));
    let pid = apply_tl_user(&mut session, &tl_enums::User::User(u)).unwrap();
    assert_eq!(pid, PeerId::from_user(UserId(10)));
    assert_eq!(session.peer_access_hash(pid), Some(0x1122_3344));

    // InputPeer / InputUser / InputDialogPeer pull access_hash from Session.
    assert_eq!(
        input_peer_for(&session, pid),
        tl_enums::InputPeer::User(tl_types::InputPeerUser {
            user_id: 10,
            access_hash: 0x1122_3344,
        })
    );
    assert_eq!(
        input_user_for(&session, UserId(10)),
        tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: 10,
            access_hash: 0x1122_3344,
        })
    );
    assert_eq!(
        input_peer_for(&session, PeerId::from_user(UserId(1))),
        tl_enums::InputPeer::Self_
    );
    assert_eq!(
        input_user_for(&session, UserId(1)),
        tl_enums::InputUser::Self_
    );

    // Min user update must NOT overwrite access_hash, contact, phone, or online status.
    let mut u_min = sample_tl_user(10, 999_999, "MinName", false);
    u_min.min = true;
    u_min.contact = false;
    u_min.phone = None;
    u_min.status = Some(tl_enums::UserStatus::Empty);
    apply_tl_user(&mut session, &tl_enums::User::User(u_min));
    let stored = session.user(UserId(10)).unwrap();
    assert_eq!(stored.access_hash, 0x1122_3344);
    assert_eq!(stored.contact_status, ContactStatus::Contact);
    assert_eq!(stored.first_name, "Alpha");
    assert!(!stored.phone.is_empty());
    assert!(stored.lastseen.is_online(1_700_000_500));

    // UserFull sets bio, birthday, personal channel, rating, and LoadedStatus::Full.
    apply_tl_user_full(
        &mut session,
        &tl_enums::UserFull::UserFull(tl_types::UserFull {
            blocked: true,
            phone_calls_available: true,
            phone_calls_private: false,
            can_pin_message: true,
            has_scheduled: false,
            video_calls_available: false,
            voice_messages_forbidden: true,
            translations_disabled: false,
            stories_pinned_available: false,
            blocked_my_stories_from: false,
            wallpaper_overridden: false,
            contact_require_premium: false,
            read_dates_private: true,
            sponsored_enabled: false,
            can_view_revenue: false,
            bot_can_manage_emoji_status: false,
            display_gifts_button: false,
            noforwards_my_enabled: false,
            noforwards_peer_enabled: false,
            unofficial_security_risk: false,
            id: 10,
            about: Some("Test bio".into()),
            settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
                report_spam: true,
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
            notify_settings: empty_notify(),
            bot_info: None,
            pinned_msg_id: None,
            common_chats_count: 3,
            folder_id: None,
            ttl_period: Some(86_400),
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
            birthday: Some(tl_enums::Birthday::Birthday(tl_types::Birthday {
                day: 15,
                month: 6,
                year: Some(1995),
            })),
            personal_channel_id: Some(700),
            personal_channel_message: Some(12),
            stargifts_count: Some(5),
            starref_program: None,
            bot_verification: None,
            send_paid_messages_stars: Some(25),
            disallowed_gifts: None,
            stars_rating: Some(tl_enums::StarsRating::StarsRating(tl_types::StarsRating {
                level: 4,
                current_level_stars: 100,
                stars: 250,
                next_level_stars: Some(500),
            })),
            stars_my_pending_rating: None,
            stars_my_pending_rating_date: None,
            main_tab: None,
            saved_music: None,
            note: None,
            bot_manager_id: None,
            wallet_card: None,
            share_theme: None,
        }),
    );
    let full_user = session.user(UserId(10)).unwrap();
    assert_eq!(full_user.common.loaded_status, LoadedStatus::Full);
    assert_eq!(full_user.common.about, "Test bio");
    assert!(full_user.flags.contains(UserFlags::BLOCKED));
    assert_eq!(full_user.common.messages_ttl, 86_400);
    assert_eq!(full_user.personal_channel_id, Some(ChannelId(700)));
    assert_eq!(full_user.stars_per_message, 25);
    assert_eq!(full_user.stars_rating.level, 4);

    // Channel + Chat migration + InputChannel / InputDialogPeer.
    let mut ch = sample_tl_channel(500, 0x5566_7788, "Supergroup", true);
    ch.forum = true;
    let ch_pid = apply_tl_chat(&mut session, &tl_enums::Chat::Channel(ch)).unwrap();
    assert_eq!(ch_pid, PeerId::from_channel(ChannelId(500)));
    assert_eq!(
        input_channel_for(&session, ChannelId(500)),
        tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
            channel_id: 500,
            access_hash: 0x5566_7788,
        })
    );
    assert_eq!(
        input_dialog_peer_for(&session, DialogKey::History(ch_pid)),
        Some(tl_enums::InputDialogPeer::InputDialogPeer(
            tl_types::InputDialogPeer {
                peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                    channel_id: 500,
                    access_hash: 0x5566_7788,
                }),
            }
        ))
    );

    // Basic chat migrated to channel 500.
    let basic_chat = tl_enums::Chat::Chat(tl_types::Chat {
        creator: true,
        left: false,
        deactivated: true,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id: 77,
        title: "Legacy Group".into(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 10,
        date: 1_600_000_000,
        version: 2,
        migrated_to: Some(tl_enums::InputChannel::InputChannel(
            tl_types::InputChannel {
                channel_id: 500,
                access_hash: 0x5566_7788,
            },
        )),
        admin_rights: None,
        default_banned_rights: None,
    });
    apply_tl_chat(&mut session, &basic_chat);
    assert_eq!(
        session.chat(ChatId(77)).unwrap().migrated_to,
        Some(ChannelId(500))
    );
    assert!(
        session
            .channel(ChannelId(500))
            .unwrap()
            .flags
            .contains(ChannelFlags::FORUM)
    );

    // ChannelFull sets about, slowmode, linked_chat_id, participants_count.
    apply_tl_chat_full(
        &mut session,
        &tl_enums::ChatFull::ChannelFull(tl_types::ChannelFull {
            can_view_participants: true,
            can_set_username: false,
            can_set_stickers: false,
            hidden_prehistory: true,
            can_set_location: false,
            has_scheduled: false,
            can_view_stats: false,
            blocked: false,
            can_delete_channel: false,
            antispam: true,
            participants_hidden: false,
            translations_disabled: false,
            stories_pinned_available: false,
            view_forum_as_messages: false,
            restricted_sponsored: false,
            can_view_revenue: false,
            paid_media_allowed: false,
            can_view_stars_revenue: false,
            paid_reactions_available: true,
            stargifts_available: false,
            paid_messages_available: false,
            has_welcome_messages: false,
            id: 500,
            about: "Channel description".into(),
            participants_count: Some(1234),
            admins_count: Some(5),
            kicked_count: Some(2),
            banned_count: Some(1),
            online_count: Some(20),
            read_inbox_max_id: 0,
            read_outbox_max_id: 0,
            unread_count: 0,
            chat_photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
            notify_settings: empty_notify(),
            exported_invite: None,
            bot_info: Vec::new(),
            migrated_from_chat_id: Some(77),
            migrated_from_max_id: Some(99),
            pinned_msg_id: None,
            stickerset: None,
            available_min_id: None,
            folder_id: None,
            linked_chat_id: Some(501),
            location: None,
            slowmode_seconds: Some(30),
            slowmode_next_send_date: None,
            stats_dc: None,
            pts: 10,
            call: None,
            ttl_period: None,
            pending_suggestions: None,
            groupcall_default_join_as: None,
            theme_emoticon: None,
            requests_pending: None,
            recent_requesters: None,
            default_send_as: None,
            available_reactions: None,
            reactions_limit: Some(11),
            stories: None,
            wallpaper: None,
            boosts_applied: Some(3),
            boosts_unrestrict: Some(2),
            emojiset: None,
            bot_verification: None,
            stargifts_count: None,
            send_paid_messages_stars: None,
            main_tab: None,
            guard_bot_id: None,
            share_theme: None,
        }),
    );
    let ch_full = session.channel(ChannelId(500)).unwrap();
    assert_eq!(ch_full.common.loaded_status, LoadedStatus::Full);
    assert_eq!(ch_full.common.about, "Channel description");
    assert_eq!(ch_full.members_count, 1234);
    assert_eq!(ch_full.discussion_link, Some(ChannelId(501)));
    assert_eq!(ch_full.mg_info.as_ref().unwrap().slowmode_seconds, 30);
}

#[test]
fn media_photo_document_webpage_and_all_20_message_media_variants() {
    // Photo with stripped thumb + progressive size.
    let p = photo_from_tl(&sample_photo(9001)).unwrap();
    assert_eq!(p.id, PhotoId(9001));
    assert!(p.has_stickers);
    assert_eq!(
        p.stripped_thumb.as_deref(),
        Some(&[0x01, 0x20, 0x20, 0xAB][..])
    );
    assert_eq!(p.sizes.len(), 2);
    assert_eq!(p.sizes[1].size, 48_000);

    // Document kinds: video, round, voice, audio, sticker, animated sticker, video sticker, gif, file.
    let d_round = document_from_tl(&sample_document(
        1,
        "video/mp4",
        vec![tl_enums::DocumentAttribute::Video(
            tl_types::DocumentAttributeVideo {
                round_message: true,
                supports_streaming: true,
                nosound: false,
                duration: 12.5,
                w: 384,
                h: 384,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            },
        )],
    ))
    .unwrap();
    assert_eq!(d_round.kind, DocumentKind::RoundVideo);
    assert_eq!(d_round.thumbs.len(), 1);
    assert!(d_round.stripped_thumb.is_some());

    let d_voice = document_from_tl(&sample_document(
        2,
        "audio/ogg",
        vec![tl_enums::DocumentAttribute::Audio(
            tl_types::DocumentAttributeAudio {
                voice: true,
                duration: 7,
                title: None,
                performer: None,
                waveform: Some(vec![1, 2, 3]),
            },
        )],
    ))
    .unwrap();
    assert_eq!(d_voice.kind, DocumentKind::Voice);

    let d_anim_sticker = document_from_tl(&sample_document(
        3,
        "application/x-tgsticker",
        vec![tl_enums::DocumentAttribute::Sticker(
            tl_types::DocumentAttributeSticker {
                mask: false,
                alt: "😀".into(),
                stickerset: tl_enums::InputStickerSet::Empty,
                mask_coords: None,
            },
        )],
    ))
    .unwrap();
    assert_eq!(d_anim_sticker.kind, DocumentKind::AnimatedSticker);
    assert_eq!(d_anim_sticker.sticker.as_ref().unwrap().alt, "😀");

    let d_gif = document_from_tl(&sample_document(
        4,
        "video/mp4",
        vec![tl_enums::DocumentAttribute::Animated],
    ))
    .unwrap();
    assert_eq!(d_gif.kind, DocumentKind::Gif);

    // WebPage conversion.
    let wp = webpage_from_tl(&tl_enums::WebPage::WebPage(tl_types::WebPage {
        has_large_media: true,
        video_cover_photo: false,
        id: 777,
        url: "https://example.org/post".into(),
        display_url: "example.org/post".into(),
        hash: 55,
        r#type: Some("article".into()),
        site_name: Some("Example".into()),
        title: Some("Title".into()),
        description: Some("Desc".into()),
        photo: Some(sample_photo(9002)),
        embed_url: None,
        embed_type: None,
        embed_width: None,
        embed_height: None,
        duration: None,
        author: Some("Author".into()),
        document: None,
        cached_page: None,
        attributes: None,
    }))
    .unwrap();
    assert_eq!(wp.title.as_deref(), Some("Title"));
    assert!(wp.photo.is_some());

    // Verify all 20 MessageMedia TL constructors convert cleanly without panicking.
    let media_cases: Vec<tl_enums::MessageMedia> = vec![
        tl_enums::MessageMedia::Empty,
        tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler: true,
            live_photo: false,
            photo: Some(sample_photo(1)),
            ttl_seconds: Some(10),
            video: None,
        }),
        tl_enums::MessageMedia::Geo(tl_types::MessageMediaGeo {
            geo: tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
                long: 13.4,
                lat: 52.5,
                access_hash: 1,
                accuracy_radius: Some(50),
            }),
        }),
        tl_enums::MessageMedia::Contact(tl_types::MessageMediaContact {
            phone_number: "+9990001".into(),
            first_name: "Alpha".into(),
            last_name: "Beta".into(),
            vcard: String::new(),
            user_id: 10,
        }),
        tl_enums::MessageMedia::Unsupported,
        tl_enums::MessageMedia::Document(tl_types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: false,
            round: false,
            voice: false,
            document: Some(sample_document(5, "application/pdf", Vec::new())),
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        }),
        tl_enums::MessageMedia::WebPage(tl_types::MessageMediaWebPage {
            force_large_media: true,
            force_small_media: false,
            manual: false,
            safe: true,
            webpage: tl_enums::WebPage::Pending(tl_types::WebPagePending {
                id: 555,
                url: Some("https://example.org".into()),
                date: 1_700_000_000,
            }),
        }),
        tl_enums::MessageMedia::Venue(tl_types::MessageMediaVenue {
            geo: tl_enums::GeoPoint::Empty,
            title: "Cafe".into(),
            address: "1 Main St".into(),
            provider: "foursquare".into(),
            venue_id: "v1".into(),
            venue_type: "food".into(),
        }),
        tl_enums::MessageMedia::Game(tl_types::MessageMediaGame {
            game: tl_enums::Game::Game(tl_types::Game {
                id: 11,
                access_hash: 22,
                short_name: "pong".into(),
                title: "Pong".into(),
                description: "Classic".into(),
                photo: sample_photo(2),
                document: None,
            }),
        }),
        tl_enums::MessageMedia::Invoice(tl_types::MessageMediaInvoice {
            shipping_address_requested: false,
            test: true,
            title: "Item".into(),
            description: "Desc".into(),
            photo: None,
            receipt_msg_id: None,
            currency: "USD".into(),
            total_amount: 100,
            start_param: "p".into(),
            extended_media: None,
        }),
        tl_enums::MessageMedia::GeoLive(tl_types::MessageMediaGeoLive {
            geo: tl_enums::GeoPoint::Empty,
            heading: Some(90),
            period: 900,
            proximity_notification_radius: Some(100),
        }),
        tl_enums::MessageMedia::Poll(tl_types::MessageMediaPoll {
            poll: tl_enums::Poll::Poll(tl_types::Poll {
                id: 88,
                closed: false,
                public_voters: true,
                multiple_choice: false,
                quiz: true,
                open_answers: false,
                revoting_disabled: false,
                shuffle_answers: false,
                hide_results_until_close: false,
                creator: false,
                subscribers_only: false,
                question: tl_enums::TextWithEntities::TextWithEntities(
                    tl_types::TextWithEntities {
                        text: "2+2?".into(),
                        entities: Vec::new(),
                    },
                ),
                answers: vec![tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "4".into(),
                            entities: Vec::new(),
                        },
                    ),
                    option: vec![0x31],
                    media: None,
                    added_by: None,
                    date: None,
                })],
                close_period: None,
                close_date: None,
                countries_iso2: None,
                hash: 0,
            }),
            results: Box::new(tl_enums::PollResults::PollResults(tl_types::PollResults {
                min: false,
                has_unread_votes: false,
                can_view_stats: false,
                results: None,
                total_voters: Some(10),
                recent_voters: None,
                solution: Some("4".into()),
                solution_entities: None,
                solution_media: None,
            })),
            attached_media: None,
        }),
        tl_enums::MessageMedia::Dice(tl_types::MessageMediaDice {
            value: 6,
            emoticon: "🎲".into(),
            game_outcome: None,
        }),
        tl_enums::MessageMedia::Story(tl_types::MessageMediaStory {
            via_mention: true,
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 10 }),
            id: 5,
            story: None,
        }),
        tl_enums::MessageMedia::Giveaway(tl_types::MessageMediaGiveaway {
            only_new_subscribers: true,
            winners_are_visible: true,
            channels: vec![500],
            countries_iso2: Some(vec!["DE".into()]),
            prize_description: Some("Prize".into()),
            quantity: 5,
            months: Some(3),
            stars: None,
            until_date: 1_700_050_000,
        }),
        tl_enums::MessageMedia::GiveawayResults(tl_types::MessageMediaGiveawayResults {
            only_new_subscribers: false,
            refunded: false,
            channel_id: 500,
            additional_peers_count: None,
            launch_msg_id: 10,
            winners_count: 2,
            unclaimed_count: 0,
            winners: vec![10, 11],
            months: Some(3),
            stars: None,
            prize_description: None,
            until_date: 1_700_050_000,
        }),
        tl_enums::MessageMedia::PaidMedia(tl_types::MessageMediaPaidMedia {
            stars_amount: 50,
            extended_media: Vec::new(),
        }),
        tl_enums::MessageMedia::ToDo(tl_types::MessageMediaToDo {
            todo: tl_enums::TodoList::TodoList(tl_types::TodoList {
                others_can_append: true,
                others_can_complete: true,
                title: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                    text: "Tasks".into(),
                    entities: Vec::new(),
                }),
                list: vec![tl_enums::TodoItem::TodoItem(tl_types::TodoItem {
                    id: 1,
                    title: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Ship".into(),
                            entities: Vec::new(),
                        },
                    ),
                })],
            }),
            completions: None,
        }),
        tl_enums::MessageMedia::VideoStream(tl_types::MessageMediaVideoStream {
            rtmp_stream: true,
            call: tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
                id: 123,
                access_hash: 456,
            }),
        }),
        tl_enums::MessageMedia::Widget(tl_types::MessageMediaWidget {
            is_copy: false,
            can_query: false,
            background_query: false,
            width: 320,
            height: 240,
            payload: vec![1, 2, 3],
            public_data: None,
            public_data_hash: None,
            chat_data: None,
            widget_id: Some(77),
        }),
    ];

    assert_eq!(media_cases.len(), 20);
    let converted: Vec<MessageMedia> = media_cases.iter().map(message_media_from_tl).collect();
    assert!(matches!(converted[0], MessageMedia::Empty));
    assert!(matches!(converted[1], MessageMedia::Photo(ref p) if p.spoiler));
    assert!(matches!(converted[12], MessageMedia::Dice(ref d) if d.value == 6));
    assert!(matches!(converted[19], MessageMedia::Widget(ref w) if w.widget_id == Some(77)));
    for item in &converted[1..] {
        assert!(!matches!(item, MessageMedia::Empty));
    }
}

#[test]
fn entities_all_25_constructors_round_trip_and_service_actions() {
    let mut session = Session::new(UserId(1));
    session.set_peer_access_hash(PeerId::from_user(UserId(42)), 7777);

    let tl_entities = vec![
        tl_enums::MessageEntity::Unknown(tl_types::MessageEntityUnknown {
            offset: 0,
            length: 1,
        }),
        tl_enums::MessageEntity::Mention(tl_types::MessageEntityMention {
            offset: 1,
            length: 2,
        }),
        tl_enums::MessageEntity::Hashtag(tl_types::MessageEntityHashtag {
            offset: 3,
            length: 2,
        }),
        tl_enums::MessageEntity::BotCommand(tl_types::MessageEntityBotCommand {
            offset: 5,
            length: 2,
        }),
        tl_enums::MessageEntity::Url(tl_types::MessageEntityUrl {
            offset: 7,
            length: 2,
        }),
        tl_enums::MessageEntity::Email(tl_types::MessageEntityEmail {
            offset: 9,
            length: 2,
        }),
        tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
            offset: 11,
            length: 2,
        }),
        tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic {
            offset: 13,
            length: 2,
        }),
        tl_enums::MessageEntity::Code(tl_types::MessageEntityCode {
            offset: 15,
            length: 2,
        }),
        tl_enums::MessageEntity::Pre(tl_types::MessageEntityPre {
            offset: 17,
            length: 2,
            language: "rust".into(),
        }),
        tl_enums::MessageEntity::TextUrl(tl_types::MessageEntityTextUrl {
            offset: 19,
            length: 2,
            url: "https://example.org".into(),
        }),
        tl_enums::MessageEntity::MentionName(tl_types::MessageEntityMentionName {
            offset: 21,
            length: 2,
            user_id: 42,
        }),
        tl_enums::MessageEntity::InputMessageEntityMentionName(
            tl_types::InputMessageEntityMentionName {
                offset: 23,
                length: 2,
                user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                    user_id: 42,
                    access_hash: 7777,
                }),
            },
        ),
        tl_enums::MessageEntity::Phone(tl_types::MessageEntityPhone {
            offset: 25,
            length: 2,
        }),
        tl_enums::MessageEntity::Cashtag(tl_types::MessageEntityCashtag {
            offset: 27,
            length: 2,
        }),
        tl_enums::MessageEntity::Underline(tl_types::MessageEntityUnderline {
            offset: 29,
            length: 2,
        }),
        tl_enums::MessageEntity::Strike(tl_types::MessageEntityStrike {
            offset: 31,
            length: 2,
        }),
        tl_enums::MessageEntity::BankCard(tl_types::MessageEntityBankCard {
            offset: 33,
            length: 2,
        }),
        tl_enums::MessageEntity::Spoiler(tl_types::MessageEntitySpoiler {
            offset: 35,
            length: 2,
        }),
        tl_enums::MessageEntity::CustomEmoji(tl_types::MessageEntityCustomEmoji {
            offset: 37,
            length: 2,
            document_id: 999,
        }),
        tl_enums::MessageEntity::Blockquote(tl_types::MessageEntityBlockquote {
            collapsed: true,
            offset: 39,
            length: 2,
        }),
        tl_enums::MessageEntity::FormattedDate(tl_types::MessageEntityFormattedDate {
            relative: true,
            short_time: true,
            long_time: false,
            short_date: true,
            long_date: false,
            day_of_week: false,
            offset: 41,
            length: 2,
            date: 1_700_000_000,
        }),
        tl_enums::MessageEntity::DiffInsert(tl_types::MessageEntityDiffInsert {
            offset: 43,
            length: 2,
        }),
        tl_enums::MessageEntity::DiffReplace(tl_types::MessageEntityDiffReplace {
            offset: 45,
            length: 2,
            old_text: "old".into(),
        }),
        tl_enums::MessageEntity::DiffDelete(tl_types::MessageEntityDiffDelete {
            offset: 47,
            length: 2,
        }),
    ];
    assert_eq!(tl_entities.len(), 25);

    let domain = entities_from_tl(&tl_entities);
    assert_eq!(domain.len(), 25);
    assert_eq!(domain[0].kind, EntityKind::Unknown);
    assert_eq!(
        domain[9].kind,
        EntityKind::Pre {
            language: "rust".into()
        }
    );
    assert_eq!(
        domain[11].kind,
        EntityKind::MentionName {
            user_id: UserId(42)
        }
    );
    assert_eq!(
        domain[19].kind,
        EntityKind::CustomEmoji {
            document_id: DocumentId(999),
            force_static: false
        }
    );
    assert_eq!(domain[20].kind, EntityKind::Blockquote { collapsed: true });

    // Round-trip domain -> TL converts MentionName into InputMessageEntityMentionName with Session access_hash!
    let back = entities_to_tl(
        &[
            TextEntity::new(0, 4, EntityKind::Bold),
            TextEntity::new(
                5,
                3,
                EntityKind::MentionName {
                    user_id: UserId(42),
                },
            ),
            TextEntity::new(
                9,
                2,
                EntityKind::CustomEmoji {
                    document_id: DocumentId(999),
                    force_static: false,
                },
            ),
        ],
        &session,
    );
    assert_eq!(back.len(), 3);
    assert_eq!(
        back[1],
        tl_enums::MessageEntity::InputMessageEntityMentionName(
            tl_types::InputMessageEntityMentionName {
                offset: 5,
                length: 3,
                user_id: tl_enums::InputUser::InputUser(tl_types::InputUser {
                    user_id: 42,
                    access_hash: 7777,
                }),
            }
        )
    );

    // Sample service actions.
    assert!(matches!(
        message_action_from_tl(&tl_enums::MessageAction::ChatCreate(tl_types::MessageActionChatCreate {
            title: "Team".into(),
            users: vec![1, 2],
        })),
        ServiceAction::ChatCreate { title, users } if title == "Team" && users.len() == 2
    ));
    assert!(matches!(
        message_action_from_tl(&tl_enums::MessageAction::PinMessage),
        ServiceAction::PinMessage
    ));
    assert!(matches!(
        message_action_from_tl(&tl_enums::MessageAction::ChatMigrateTo(
            tl_types::MessageActionChatMigrateTo { channel_id: 500 }
        )),
        ServiceAction::ChatMigrateTo {
            channel_id: ChannelId(500)
        }
    ));
    assert!(matches!(
        message_action_from_tl(&tl_enums::MessageAction::PhoneCall(
            tl_types::MessageActionPhoneCall {
                video: true,
                call_id: 99,
                reason: Some(tl_enums::PhoneCallDiscardReason::Busy),
                duration: Some(45),
            }
        )),
        ServiceAction::PhoneCall {
            video: true,
            duration: Some(45),
            ..
        }
    ));

    // Message + MessageService + MessageEmpty conversions.
    let tl_msg = sample_tl_message(
        tl_enums::Peer::User(tl_types::PeerUser { user_id: 10 }),
        Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 10 })),
        55,
        1_700_000_010,
        "Hello world",
    );
    let dom_msg = message_from_tl(&tl_msg, UserId(1)).unwrap();
    assert_eq!(dom_msg.id, MsgId(55));
    assert_eq!(dom_msg.text().unwrap().text, "Hello world");
    assert!(!dom_msg.flags.contains(MessageFlags::OUTGOING));
    assert!(apply_tl_message(&mut session, &tl_msg).is_some());
    assert!(
        session
            .message(FullMsgId::new(PeerId::from_user(UserId(10)), MsgId(55)))
            .is_some()
    );

    assert!(
        message_from_tl(
            &tl_enums::Message::Empty(tl_types::MessageEmpty {
                id: 56,
                peer_id: None
            }),
            UserId(1)
        )
        .is_none()
    );
}

#[test]
fn dialog_folder_filter_draft_reply_and_typing_conversions() {
    let mut session = Session::new(UserId(1));
    let u_pid = PeerId::from_user(UserId(10));
    session.set_peer_access_hash(u_pid, 12345);

    // Draft TL <-> domain round-trip.
    let draft = Draft {
        date: 1_700_000_100,
        text: TextWithEntities::new("Draft body", vec![TextEntity::new(0, 5, EntityKind::Bold)]),
        reply: Some(ReplyInfo {
            reply_to_msg_id: Some(MsgId(42)),
            ..ReplyInfo::default()
        }),
        ..Draft::default()
    };
    let save_req = draft_to_save_request(&draft, input_peer_for(&session, u_pid), &session);
    assert_eq!(save_req.message, "Draft body");
    assert!(save_req.entities.is_some());
    assert!(save_req.reply_to.is_some());

    let parsed_draft = draft_from_tl(&tl_enums::DraftMessage::DraftMessage(
        tl_types::DraftMessage {
            no_webpage: false,
            invert_media: false,
            reply_to: input_reply_to_from_reply_info(draft.reply.as_ref().unwrap(), &session),
            message: "Draft body".into(),
            entities: save_req.entities.clone(),
            media: None,
            date: 1_700_000_100,
            effect: None,
            suggested_post: None,
            rich_message: None,
        },
    ))
    .unwrap();
    assert_eq!(parsed_draft.text.text, "Draft body");
    assert_eq!(
        parsed_draft.reply.as_ref().unwrap().reply_to_msg_id,
        Some(MsgId(42))
    );

    // Dialog + DialogFolder application.
    let dlg = sample_tl_dialog(tl_enums::Peer::User(tl_types::PeerUser { user_id: 10 }), 55);
    assert!(apply_tl_dialog(&mut session, &dlg, 1_700_000_200).is_some());
    assert_eq!(
        session.history(u_pid).unwrap().top_message_id,
        Some(MsgId(55))
    );

    let archive_dlg = tl_enums::Dialog::Folder(tl_types::DialogFolder {
        pinned: true,
        folder: tl_enums::Folder::Folder(tl_types::Folder {
            autofill_new_broadcasts: false,
            autofill_public_groups: false,
            autofill_new_correspondents: false,
            id: ARCHIVE_FOLDER_ID,
            title: "Archived Chats".into(),
            photo: None,
        }),
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 10 }),
        top_message: 55,
        unread_muted_peers_count: 1,
        unread_unmuted_peers_count: 2,
        unread_muted_messages_count: 3,
        unread_unmuted_messages_count: 4,
    });
    apply_tl_dialog(&mut session, &archive_dlg, 1_700_000_200);
    assert_eq!(session.archive().name, "Archived Chats");
    assert!(session.archive().fixed_on_top);

    // DialogFilter & Chatlist conversions.
    let f_tl = tl_enums::DialogFilter::DialogFilter(tl_types::DialogFilter {
        contacts: true,
        non_contacts: false,
        groups: true,
        broadcasts: false,
        bots: false,
        exclude_muted: true,
        exclude_read: false,
        exclude_archived: false,
        title_noanimate: false,
        id: 2,
        title: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
            text: "Work".into(),
            entities: Vec::new(),
        }),
        emoticon: Some("💼".into()),
        color: Some(3),
        pinned_peers: vec![input_peer_for(&session, u_pid)],
        include_peers: vec![input_peer_for(&session, u_pid)],
        exclude_peers: Vec::new(),
    });
    let filter = dialog_filter_from_tl(&f_tl, Some(UserId(1))).unwrap();
    assert_eq!(filter.id, 2);
    assert!(
        filter
            .flags
            .contains(ChatFilterFlags::CONTACTS | ChatFilterFlags::NO_MUTED)
    );
    assert_eq!(filter.pinned, vec![u_pid]);
    assert!(dialog_filter_from_tl(&tl_enums::DialogFilter::Default, Some(UserId(1))).is_none());

    // TypingAction + PeerId sanity.
    assert_eq!(
        typing_action_from_tl(&tl_enums::SendMessageAction::SendMessageUploadPhotoAction(
            tl_types::SendMessageUploadPhotoAction { progress: 65 }
        )),
        TypingAction::UploadPhoto(65)
    );
    assert_eq!(
        typing_action_from_tl(
            &tl_enums::SendMessageAction::SendMessageUploadDocumentAction(
                tl_types::SendMessageUploadDocumentAction { progress: 40 }
            )
        ),
        TypingAction::UploadDocument(40)
    );
    assert_eq!(
        peer_id_from_tl(&tl_enums::Peer::Channel(tl_types::PeerChannel {
            channel_id: 500
        })),
        PeerId::from_channel(ChannelId(500))
    );
    let _ = ReactionId::Paid;
}
