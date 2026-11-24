//! Comprehensive unit and integration tests for `tg-stories`.
//!
//! Covers:
//! - TL conversions (`StoryItem`, `PeerStories`, `stories.AllStories`, `updateStory`,
//!   `updateReadStories`, `updateStoriesStealthMode`, `MediaArea`, `StoryViewsList`,
//!   `StoryReactionsList`, `StoryAlbum`, RPC request round-trips)
//! - Per-peer source ordering (self, unread, premium, service user, timestamp, hidden/archived)
//! - Read-state batching timers (`readStories` 3 s, `incrementStoryViews` 5 s, `getPeerMaxIDs`,
//!   `getAllReadPeerStories`, `is_quit_prevent`, expiry & polling timers)
//! - Preload queue prioritization and cancellation
//! - Album ID lists, `respecting_pinned`, sparse windowing, reordering, pin limits
//! - Viewer state machine (5 s photo auto-advance, pause on hold/hover/menus/caption/reply,
//!   video tracking, live video stream, unsupported placeholder, navigation edge cases,
//!   unread-only cached sources, sibling previews, reply visibility, stealth mode, area hit-test)
//! - Dialogs stories top bar (ring segments, self unread highlight, hysteresis, collapsed strip)

use tg_data::{
    ChannelId, DocumentData, DocumentId, DocumentKind, EntityKind, FullMsgId, FullStoryId, MsgId,
    PeerId, PhotoData, PhotoId, ReactionId, StoryId, TextEntity, TextWithEntities, TimeId, UserId,
};
use tg_stories::*;
use tg_tl::{Deserializable, Serializable, enums as tl_enums, types as tl_types};

const SELF_USER: PeerId = PeerId::from_user(UserId(100));
const PEER_ALPHA: PeerId = PeerId::from_user(UserId(201));
const PEER_BETA: PeerId = PeerId::from_user(UserId(202));
const PEER_GAMMA: PeerId = PeerId::from_user(UserId(203));
const PEER_DELTA: PeerId = PeerId::from_user(UserId(204));
const CHANNEL_ONE: PeerId = PeerId::from_channel(ChannelId(5001));
const MEGAGROUP_ONE: PeerId = PeerId::from_channel(ChannelId(5002));

fn make_photo(id: u64) -> PhotoData {
    PhotoData {
        id: PhotoId(id),
        access_hash: 42,
        dc_id: 2,
        ..PhotoData::default()
    }
}

fn make_video(id: u64, duration_secs: f64) -> DocumentData {
    DocumentData {
        id: DocumentId(id),
        access_hash: 99,
        mime_type: "video/mp4".to_string(),
        kind: DocumentKind::Video,
        duration: Some(duration_secs),
        width: Some(720),
        height: Some(1280),
        ..DocumentData::default()
    }
}

fn make_photo_story(peer_id: PeerId, id: StoryId, date: TimeId, expires: TimeId) -> Story {
    Story {
        id,
        peer_id,
        date,
        expires,
        last_update_time: date,
        media: StoryMedia::Photo(make_photo(id as u64)),
        caption: TextWithEntities::plain(format!("Caption #{id}")),
        out: peer_id == SELF_USER,
        in_profile: false,
        pinned_to_top: false,
        privacy: StoryPrivacy::Public,
        privacy_allowed: Vec::new(),
        privacy_disallowed: Vec::new(),
        no_forwards: false,
        edited: false,
        from_peer: None,
        repost: StoryRepostInfo::default(),
        music: None,
        album_ids: Default::default(),
        sent_reaction: None,
        recent_viewers: Vec::new(),
        views: StoryViews::default(),
        channel_reactions: StoryViews::default(),
        locations: Vec::new(),
        suggested_reactions: Vec::new(),
        channel_posts: Vec::new(),
        url_areas: Vec::new(),
        weather_areas: Vec::new(),
    }
}

fn make_contact_meta(premium: bool, hidden: bool) -> PeerStoryMeta {
    PeerStoryMeta {
        ctx: StoryPeerContext {
            is_user: true,
            is_premium: premium,
            has_username: true,
            ..StoryPeerContext::default()
        },
        stories_hidden: hidden,
        is_contact: true,
        ..PeerStoryMeta::default()
    }
}

fn add_peer_source(
    store: &mut StoriesStore,
    peer_id: PeerId,
    stories: Vec<Story>,
    max_read_id: StoryId,
    now: TimeId,
) {
    let items: Vec<ParsedStoryItem> = stories
        .into_iter()
        .map(|s| ParsedStoryItem::Full(Box::new(s)))
        .collect();
    store.apply_peer_stories(
        ParsedPeerStories {
            peer_id,
            max_read_id,
            items,
            has_video_stream: false,
        },
        ParseSource::MyStrip,
        now,
    );
}

fn tl_photo_media(photo_id: i64) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
            has_stickers: false,
            id: photo_id,
            access_hash: 777,
            file_reference: vec![1, 2, 3],
            date: 1_700_000_000,
            sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "x".to_string(),
                w: 720,
                h: 1280,
                size: 50_000,
            })],
            video_sizes: None,
            dc_id: 2,
        })),
        ttl_seconds: None,
        video: None,
    })
}

fn tl_coords(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    rotation: f64,
    radius: Option<f64>,
) -> tl_enums::MediaAreaCoordinates {
    tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(tl_types::MediaAreaCoordinates {
        x,
        y,
        w,
        h,
        rotation,
        radius,
    })
}

// ===========================================================================
// 1. TL Conversion & Wire Round-Trip Tests
// ===========================================================================

#[test]
fn tl_story_item_areas_entities_repost_and_strip_links() {
    let now = 1_700_000_100;
    let raw_item = tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: true,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited: true,
        contacts: false,
        selected_contacts: false,
        out: false,
        id: 55,
        date: 1_700_000_000,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 999 })),
        fwd_from: Some(tl_enums::StoryFwdHeader::StoryFwdHeader(
            tl_types::StoryFwdHeader {
                modified: true,
                from: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                    channel_id: 5001,
                })),
                from_name: Some("Channel News".to_string()),
                story_id: Some(12),
            },
        )),
        expire_date: 1_700_086_400,
        caption: Some("Visit https://example.com #tag @user bold".to_string()),
        entities: Some(vec![
            tl_enums::MessageEntity::Url(tl_types::MessageEntityUrl {
                offset: 6,
                length: 19,
            }),
            tl_enums::MessageEntity::Hashtag(tl_types::MessageEntityHashtag {
                offset: 26,
                length: 4,
            }),
            tl_enums::MessageEntity::Mention(tl_types::MessageEntityMention {
                offset: 31,
                length: 5,
            }),
            tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                offset: 37,
                length: 4,
            }),
        ]),
        media: Box::new(tl_photo_media(1001)),
        media_areas: Some(vec![
            tl_enums::MediaArea::GeoPoint(tl_types::MediaAreaGeoPoint {
                coordinates: tl_coords(50.0, 50.0, 20.0, 10.0, 15.0, Some(5.0)),
                geo: tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
                    long: 2.3522,
                    lat: 48.8566,
                    access_hash: 1,
                    accuracy_radius: Some(10),
                }),
                address: Some(tl_enums::GeoPointAddress::GeoPointAddress(
                    tl_types::GeoPointAddress {
                        country_iso2: "FR".to_string(),
                        state: Some("IDF".to_string()),
                        city: Some("CityOne".to_string()),
                        street: Some("Main St".to_string()),
                    },
                )),
            }),
            tl_enums::MediaArea::Venue(tl_types::MediaAreaVenue {
                coordinates: tl_coords(30.0, 40.0, 20.0, 10.0, 0.0, None),
                geo: tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
                    long: 10.0,
                    lat: 20.0,
                    access_hash: 2,
                    accuracy_radius: None,
                }),
                title: "Coffee Hub".to_string(),
                address: "123 Road".to_string(),
                provider: "foursquare".to_string(),
                venue_id: "v1".to_string(),
                venue_type: "cafe".to_string(),
            }),
            tl_enums::MediaArea::SuggestedReaction(tl_types::MediaAreaSuggestedReaction {
                dark: true,
                flipped: true,
                coordinates: tl_coords(70.0, 30.0, 15.0, 15.0, -10.0, None),
                reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "\u{1F525}".to_string(),
                }),
            }),
            tl_enums::MediaArea::ChannelPost(tl_types::MediaAreaChannelPost {
                coordinates: tl_coords(50.0, 75.0, 40.0, 20.0, 0.0, None),
                channel_id: 5001,
                msg_id: 88,
            }),
            tl_enums::MediaArea::Url(tl_types::MediaAreaUrl {
                coordinates: tl_coords(50.0, 20.0, 30.0, 8.0, 0.0, None),
                url: "https://example.org".to_string(),
            }),
            tl_enums::MediaArea::StarGift(tl_types::MediaAreaStarGift {
                coordinates: tl_coords(20.0, 80.0, 20.0, 20.0, 0.0, None),
                slug: "StarSample-1".to_string(),
            }),
            tl_enums::MediaArea::Weather(tl_types::MediaAreaWeather {
                coordinates: tl_coords(80.0, 80.0, 20.0, 10.0, 0.0, Some(8.0)),
                emoji: "\u{2600}".to_string(),
                temperature_c: 23.45,
                color: 0x11223344,
            }),
        ]),
        privacy: Some(vec![
            tl_enums::PrivacyRule::PrivacyValueAllowUsers(tl_types::PrivacyValueAllowUsers {
                users: vec![201, 202],
            }),
            tl_enums::PrivacyRule::PrivacyValueDisallowUsers(tl_types::PrivacyValueDisallowUsers {
                users: vec![203],
            }),
        ]),
        views: Some(tl_enums::StoryViews::StoryViews(tl_types::StoryViews {
            has_viewers: true,
            views_count: 100,
            forwards_count: Some(5),
            reactions: Some(vec![tl_enums::ReactionCount::ReactionCount(
                tl_types::ReactionCount {
                    chosen_order: Some(0),
                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "\u{1F525}".to_string(),
                    }),
                    count: 12,
                },
            )]),
            reactions_count: Some(12),
            recent_viewers: Some(vec![201, 202, 203, 204]),
        })),
        sent_reaction: Some(tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: "\u{1F525}".to_string(),
        })),
        albums: Some(vec![7, 9]),
        music: None,
    });

    // Non-premium non-verified user -> links stripped, bold kept.
    let regular_ctx = StoryPeerContext {
        is_user: true,
        is_verified: false,
        is_premium: false,
        ..StoryPeerContext::default()
    };
    let parsed_user = story_item_from_tl(PEER_ALPHA, regular_ctx, &raw_item, now, None).unwrap();
    let ParsedStoryItem::Full(story_u) = parsed_user else {
        panic!("expected Full story");
    };
    assert_eq!(story_u.caption.entities.len(), 1);
    assert_eq!(story_u.caption.entities[0].kind, EntityKind::Bold);
    // Recent viewers clamped to RECENT_VIEWERS_MAX (3).
    assert_eq!(story_u.recent_viewers.len(), 3);
    assert_eq!(story_u.locations.len(), 2);
    assert_eq!(story_u.locations[0].country_iso2, "FR");
    assert_eq!(story_u.locations[1].title, "Coffee Hub");
    assert_eq!(story_u.channel_posts.len(), 1);
    assert_eq!(
        story_u.channel_posts[0].item_id,
        FullMsgId::new(CHANNEL_ONE, MsgId::from(88))
    );
    assert_eq!(story_u.url_areas.len(), 2);
    assert_eq!(story_u.url_areas[0].url, "https://example.org");
    assert_eq!(story_u.url_areas[1].url, "tg://nft?slug=StarSample-1");
    assert_eq!(story_u.weather_areas.len(), 1);
    assert_eq!(story_u.weather_areas[0].millicelsius, 23_450);
    assert!((story_u.weather_areas[0].celsius() - 23.45).abs() < 1e-6);
    assert!(story_u.is_repost());
    assert!(story_u.repost.modified);
    assert_eq!(story_u.repost.source_peer, Some(CHANNEL_ONE));
    assert_eq!(story_u.repost.source_id, 12);
    assert_eq!(story_u.from_peer, Some(PeerId::from_user(UserId(999))));
    assert!(story_u.album_ids.contains(&7) && story_u.album_ids.contains(&9));
    assert_eq!(story_u.privacy_allowed, vec![PEER_ALPHA, PEER_BETA]);
    assert_eq!(story_u.privacy_disallowed, vec![PEER_GAMMA]);

    // Channel story -> keeps all 4 entities and populates suggested reaction counts from views.reactions.
    let channel_ctx = StoryPeerContext {
        is_channel: true,
        is_broadcast: true,
        ..StoryPeerContext::default()
    };
    let parsed_ch = story_item_from_tl(CHANNEL_ONE, channel_ctx, &raw_item, now, None).unwrap();
    let ParsedStoryItem::Full(story_ch) = parsed_ch else {
        panic!("expected Full story");
    };
    assert_eq!(story_ch.caption.entities.len(), 4);
    assert_eq!(story_ch.suggested_reactions.len(), 1);
    assert_eq!(story_ch.suggested_reactions[0].count, 12);
    assert!(story_ch.suggested_reactions[0].dark);
    assert!(story_ch.suggested_reactions[0].flipped);

    // StoryArea round-trip conversion.
    let area = story_u.locations[0].area;
    let tl_roundtrip = story_area_to_tl(area);
    let parsed_back = story_area_from_tl(&tl_roundtrip);
    assert!((parsed_back.geometry.x - area.geometry.x).abs() < 1e-9);
    assert!((parsed_back.geometry.y - area.geometry.y).abs() < 1e-9);
    assert!((parsed_back.geometry.width - area.geometry.width).abs() < 1e-9);
    assert!((parsed_back.geometry.height - area.geometry.height).abs() < 1e-9);
    assert!((parsed_back.rotation - 15.0).abs() < 1e-9);
    assert!((parsed_back.radius - 5.0).abs() < 1e-9);

    // Invalid coordinates (> 100) produce empty area.
    let invalid = StoryArea::from_tl_coords(120.0, 50.0, 10.0, 10.0, 0.0, None);
    assert!(invalid.geometry.is_empty());
}

#[test]
fn tl_min_story_preserves_existing_personal_fields() {
    let now = 1_700_000_000;
    let mut existing = make_photo_story(PEER_ALPHA, 10, now - 10, now + 3600);
    existing.out = true;
    existing.sent_reaction = Some(ReactionId::Emoji("\u{2764}".to_string()));
    existing.privacy_allowed = vec![PEER_BETA];

    let min_item = tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: false,
        public: true,
        close_friends: false,
        min: true,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out: false,
        id: 10,
        date: now - 10,
        from_id: None,
        fwd_from: None,
        expire_date: now + 3600,
        caption: Some("Updated".to_string()),
        entities: None,
        media: Box::new(tl_photo_media(10)),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    });

    let ParsedStoryItem::Full(parsed) = story_item_from_tl(
        PEER_ALPHA,
        make_contact_meta(true, false).ctx,
        &min_item,
        now,
        Some(&existing),
    )
    .unwrap() else {
        panic!("expected Full");
    };
    assert!(parsed.out);
    assert_eq!(
        parsed.sent_reaction,
        Some(ReactionId::Emoji("\u{2764}".to_string()))
    );
    assert_eq!(parsed.privacy_allowed, vec![PEER_BETA]);
}

#[test]
fn tl_all_stories_pagination_and_updates() {
    let mut store = StoriesStore::new(SELF_USER);
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));
    store.set_peer_meta(PEER_BETA, make_contact_meta(true, false));

    let now = 1_700_000_000;
    // Page 1 request has no state / next=false.
    store.load_more(StorySourcesList::NotHidden);
    let actions = store.take_actions();
    assert_eq!(
        actions,
        vec![StoriesAction::GetAllStories {
            list: StorySourcesList::NotHidden,
            state: None,
        }]
    );

    let req1 = build_get_all_stories(false, None);
    assert!(!req1.next && !req1.hidden && req1.state.is_none());
    let req1_bytes = req1.to_bytes();
    assert_eq!(
        tg_tl::functions::stories::GetAllStories::from_bytes(&req1_bytes).unwrap(),
        req1
    );

    // Apply page 1 response with has_more=true, state="page1".
    let page1 = tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
        has_more: true,
        count: 2,
        state: "page1".to_string(),
        peer_stories: vec![tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 }),
            max_read_id: Some(0),
            stories: vec![tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
                pinned: false,
                public: true,
                close_friends: false,
                min: false,
                noforwards: false,
                edited: false,
                contacts: false,
                selected_contacts: false,
                out: false,
                id: 1,
                date: now - 100,
                from_id: None,
                fwd_from: None,
                expire_date: now + 3600,
                caption: None,
                entities: None,
                media: Box::new(tl_photo_media(1)),
                media_areas: None,
                privacy: None,
                views: None,
                sent_reaction: None,
                albums: None,
                music: None,
            })],
        })],
        chats: Vec::new(),
        users: Vec::new(),
        stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
            tl_types::StoriesStealthMode {
                active_until_date: Some(now + 1500),
                cooldown_until_date: Some(now + 3600),
            },
        ),
    });
    store.apply_tl_all_stories(StorySourcesList::NotHidden, &page1, now);
    assert!(store.sources_known(StorySourcesList::NotHidden));
    assert!(!store.sources_loaded(StorySourcesList::NotHidden));
    assert_eq!(store.sources_state(StorySourcesList::NotHidden), "page1");
    assert_eq!(store.stealth_mode().enabled_till, now + 1500);

    // Page 2 request passes state="page1" (and TL builder sets next=true).
    store.load_more(StorySourcesList::NotHidden);
    let actions = store.take_actions();
    assert_eq!(
        actions,
        vec![StoriesAction::GetAllStories {
            list: StorySourcesList::NotHidden,
            state: Some("page1".to_string()),
        }]
    );
    let req2 = build_get_all_stories(false, Some("page1"));
    assert!(req2.next && req2.state.as_deref() == Some("page1"));

    // Apply page 2 with has_more=false.
    let page2 = tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
        has_more: false,
        count: 2,
        state: "page2".to_string(),
        peer_stories: vec![tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 202 }),
            max_read_id: None,
            stories: vec![tl_enums::StoryItem::Skipped(tl_types::StoryItemSkipped {
                close_friends: true,
                live: false,
                id: 5,
                date: now - 50,
                expire_date: now + 3600,
            })],
        })],
        chats: Vec::new(),
        users: Vec::new(),
        stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
            tl_types::StoriesStealthMode {
                active_until_date: None,
                cooldown_until_date: None,
            },
        ),
    });
    store.apply_tl_all_stories(StorySourcesList::NotHidden, &page2, now);
    assert!(store.sources_loaded(StorySourcesList::NotHidden));
    // Further load_more is a no-op once loaded.
    store.take_actions();
    store.load_more(StorySourcesList::NotHidden);
    assert!(store.take_actions().is_empty());

    // Apply TL updates: updateReadStories, updateStoriesStealthMode, updateStory.
    let upd_read = tl_enums::Update::ReadStories(tl_types::UpdateReadStories {
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 }),
        max_id: 1,
    });
    assert!(store.apply_tl_update(&upd_read, now));
    assert_eq!(store.read_till(PEER_ALPHA), 1);
    assert_eq!(store.source(PEER_ALPHA).unwrap().unread_count(), 0);

    let upd_stealth = tl_enums::Update::StoriesStealthMode(tl_types::UpdateStoriesStealthMode {
        stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
            tl_types::StoriesStealthMode {
                active_until_date: Some(now + 900),
                cooldown_until_date: Some(now + 7200),
            },
        ),
    });
    assert!(store.apply_tl_update(&upd_stealth, now));
    assert_eq!(store.stealth_mode().enabled_till, now + 900);
    assert_eq!(store.stealth_mode().cooldown_till, now + 7200);

    let upd_story = tl_enums::Update::Story(tl_types::UpdateStory {
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 }),
        story: tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
            pinned: false,
            public: true,
            close_friends: false,
            min: false,
            noforwards: false,
            edited: false,
            contacts: false,
            selected_contacts: false,
            out: false,
            id: 2,
            date: now,
            from_id: None,
            fwd_from: None,
            expire_date: now + 3600,
            caption: None,
            entities: None,
            media: Box::new(tl_photo_media(2)),
            media_areas: None,
            privacy: None,
            views: None,
            sent_reaction: None,
            albums: None,
            music: None,
        }),
    });
    assert!(store.apply_tl_update(&upd_story, now));
    assert_eq!(store.source(PEER_ALPHA).unwrap().unread_count(), 1);
}

// ===========================================================================
// 2. Per-Peer Source Ordering & Hidden Sources Tests
// ===========================================================================

#[test]
fn sources_ordering_self_unread_premium_service_and_timestamp() {
    let mut store = StoriesStore::new(SELF_USER);
    let now = 1_700_000_000;

    // Regular unread (older)
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));
    // Premium unread (older than Alpha, should still sort BEFORE Alpha!)
    store.set_peer_meta(PEER_BETA, make_contact_meta(true, false));
    // Regular read (newest timestamp, should still sort AFTER all unread!)
    store.set_peer_meta(PEER_GAMMA, make_contact_meta(false, false));
    // Premium read (should sort above regular read, below all unread!)
    store.set_peer_meta(PEER_DELTA, make_contact_meta(true, false));
    // Service notifications (unread, gets 1<<47 boost like premium!)
    let service_id = PeerId::SERVICE_NOTIFICATIONS;
    store.set_peer_meta(
        service_id,
        PeerStoryMeta {
            ctx: StoryPeerContext {
                is_user: true,
                is_service_user: true,
                ..StoryPeerContext::default()
            },
            ..PeerStoryMeta::default()
        },
    );

    add_peer_source(
        &mut store,
        PEER_GAMMA,
        vec![make_photo_story(PEER_GAMMA, 1, now - 1, now + 3600)],
        1, // all read
        now,
    );
    add_peer_source(
        &mut store,
        PEER_ALPHA,
        vec![make_photo_story(PEER_ALPHA, 1, now - 10, now + 3600)],
        0, // unread
        now,
    );
    add_peer_source(
        &mut store,
        PEER_DELTA,
        vec![make_photo_story(PEER_DELTA, 1, now - 50, now + 3600)],
        1, // all read
        now,
    );
    add_peer_source(
        &mut store,
        PEER_BETA,
        vec![make_photo_story(PEER_BETA, 1, now - 200, now + 3600)],
        0, // unread premium
        now,
    );
    add_peer_source(
        &mut store,
        service_id,
        vec![make_photo_story(service_id, 1, now - 100, now + 3600)],
        0, // unread service (newer than Beta -> before Beta!)
        now,
    );
    add_peer_source(
        &mut store,
        SELF_USER,
        vec![make_photo_story(SELF_USER, 1, now - 500, now + 3600)],
        0, // self always read_till = max_id, yet ALWAYS FIRST!
        now,
    );

    let ordered: Vec<PeerId> = store
        .sources(StorySourcesList::NotHidden)
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(
        ordered,
        vec![
            SELF_USER,  // 1<<50 (always first)
            service_id, // unread (1<<49) + service (1<<47) + (now - 100)
            PEER_BETA,  // unread (1<<49) + premium (1<<47) + (now - 200)
            PEER_ALPHA, // unread (1<<49) + (now - 10)
            PEER_DELTA, // read + premium (1<<47) + (now - 50)
            PEER_GAMMA, // read + (now - 1)
        ]
    );

    // Marking PEER_ALPHA read moves it below PEER_DELTA (premium read) and below PEER_GAMMA (newer read).
    store.apply_read_stories(PEER_ALPHA, 1);
    let ordered_after: Vec<PeerId> = store
        .sources(StorySourcesList::NotHidden)
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(
        ordered_after,
        vec![
            SELF_USER, service_id, PEER_BETA, PEER_DELTA, PEER_GAMMA, PEER_ALPHA,
        ]
    );

    // Hiding PEER_BETA moves it from NotHidden -> Hidden and updates hidden counts.
    store.take_events();
    store.toggle_hidden(PEER_BETA, true);
    assert!(
        store
            .sources(StorySourcesList::NotHidden)
            .iter()
            .all(|s| s.id != PEER_BETA)
    );
    assert_eq!(store.sources(StorySourcesList::Hidden).len(), 1);
    assert_eq!(store.sources(StorySourcesList::Hidden)[0].id, PEER_BETA);
    assert!(
        store
            .take_events()
            .contains(&StoriesEvent::HiddenCountsChanged {
                total: 1,
                unread: 1,
            })
    );
}

// ===========================================================================
// 3. Read-State Batching Timers & Polling Tests
// ===========================================================================

#[test]
fn read_stories_batching_timers_and_peer_switch_flush() {
    let mut store = StoriesStore::new(SELF_USER);
    let now_secs = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));
    store.set_peer_meta(PEER_BETA, make_contact_meta(false, false));

    add_peer_source(
        &mut store,
        PEER_ALPHA,
        vec![
            make_photo_story(PEER_ALPHA, 1, now_secs - 30, now_secs + 3600),
            make_photo_story(PEER_ALPHA, 2, now_secs - 20, now_secs + 3600),
            make_photo_story(PEER_ALPHA, 3, now_secs - 10, now_secs + 3600),
        ],
        0,
        now_secs,
    );
    add_peer_source(
        &mut store,
        PEER_BETA,
        vec![make_photo_story(
            PEER_BETA,
            10,
            now_secs - 5,
            now_secs + 3600,
        )],
        0,
        now_secs,
    );
    store.take_actions();

    // Mark Peer Alpha story 1 at t = 1000ms -> schedules 3s timer (deadline 4000ms).
    store.mark_as_read(FullStoryId::new(PEER_ALPHA, 1), now_secs, 1_000);
    assert!(store.take_actions().is_empty());
    assert_eq!(store.next_timer_delay_ms(now_secs, 1_000), Some(3_000));

    // Mark Peer Alpha story 2 at t = 2500ms -> resets 3s timer (deadline 5500ms).
    store.mark_as_read(FullStoryId::new(PEER_ALPHA, 2), now_secs, 2_500);
    assert!(store.take_actions().is_empty());

    // At t = 4000ms timer has NOT fired yet because it was debounced to 5500ms.
    store.tick(now_secs, 4_000);
    assert!(store.take_actions().is_empty());

    // At t = 5500ms -> flushes single batched ReadStories(PEER_ALPHA, max_id=2)!
    store.tick(now_secs, 5_500);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::ReadStories {
            peer_id: PEER_ALPHA,
            max_id: 2,
        }]
    );

    // While PEER_ALPHA readStories is still in-flight, marking story 3 queues it until finished.
    store.mark_as_read(FullStoryId::new(PEER_ALPHA, 3), now_secs, 6_000);
    store.tick(now_secs, 9_000);
    assert!(store.take_actions().is_empty()); // still in flight!
    store.finish_mark_as_read(PEER_ALPHA);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::ReadStories {
            peer_id: PEER_ALPHA,
            max_id: 3,
        }]
    );
    store.finish_mark_as_read(PEER_ALPHA);

    // Now test cross-peer immediate flush:
    // Add story 4 to Peer Alpha, mark read -> pending; then mark Peer Beta story 10 ->
    // Peer Alpha flushes immediately, Peer Beta waits 3s!
    store.apply_story_update(
        PEER_ALPHA,
        ParsedStoryItem::Full(Box::new(make_photo_story(
            PEER_ALPHA,
            4,
            now_secs,
            now_secs + 3600,
        ))),
        now_secs,
    );
    store.take_actions();
    store.mark_as_read(FullStoryId::new(PEER_ALPHA, 4), now_secs, 10_000);
    assert!(store.take_actions().is_empty());

    store.mark_as_read(FullStoryId::new(PEER_BETA, 10), now_secs, 10_500);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::ReadStories {
            peer_id: PEER_ALPHA,
            max_id: 4,
        }]
    );
    // Peer Beta is flushed by is_quit_prevent() before shutdown.
    assert!(store.is_quit_prevent());
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::ReadStories {
            peer_id: PEER_BETA,
            max_id: 10,
        }]
    );
    store.finish_mark_as_read(PEER_ALPHA);
    store.finish_mark_as_read(PEER_BETA);
    assert!(!store.is_quit_prevent());
}

#[test]
fn increment_views_and_get_peer_max_ids_batching_timers() {
    let mut store = StoriesStore::new(SELF_USER);
    let now_secs = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));

    // Expired in-profile stories trigger 5s batched IncrementStoryViews.
    let mut s1 = make_photo_story(PEER_ALPHA, 1, now_secs - 90_000, now_secs - 100);
    s1.in_profile = true;
    let mut s2 = make_photo_story(PEER_ALPHA, 2, now_secs - 80_000, now_secs - 50);
    s2.in_profile = true;
    store.insert_story(s1, now_secs);
    store.insert_story(s2, now_secs);
    store.take_actions();

    store.mark_as_read(FullStoryId::new(PEER_ALPHA, 1), now_secs, 1_000);
    store.mark_as_read(FullStoryId::new(PEER_ALPHA, 2), now_secs, 2_000);
    store.tick(now_secs, 5_999);
    assert!(store.take_actions().is_empty());
    store.tick(now_secs, 6_000);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::IncrementStoryViews {
            peer_id: PEER_ALPHA,
            ids: vec![1, 2],
        }]
    );

    // Polling stories.getPeerMaxIDs batches multiple peers over 500ms window.
    store.schedule_peer_max_id_poll(PEER_ALPHA, 10_000);
    store.schedule_peer_max_id_poll(PEER_BETA, 10_200);
    store.tick(now_secs, 10_499);
    assert!(store.take_actions().is_empty());
    store.tick(now_secs, 10_500);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::GetPeerMaxIds {
            peer_ids: vec![PEER_ALPHA, PEER_BETA],
        }]
    );

    // Applying max IDs triggers GetPeerStories for peers with newer stories.
    store.apply_peer_max_ids(&[
        (
            PEER_ALPHA,
            RecentStoryInfo {
                max_id: 99,
                live: false,
            },
        ),
        (
            PEER_BETA,
            RecentStoryInfo {
                max_id: 0,
                live: false,
            },
        ),
    ]);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::GetPeerStories {
            peer_id: PEER_ALPHA,
        }]
    );
}

#[test]
fn expiration_timer_and_preload_priority() {
    let mut store = StoriesStore::new(SELF_USER);
    let now = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));
    store.set_peer_meta(PEER_BETA, make_contact_meta(false, false));

    add_peer_source(
        &mut store,
        PEER_ALPHA,
        vec![make_photo_story(PEER_ALPHA, 1, now - 100, now + 60)],
        0,
        now,
    );
    add_peer_source(
        &mut store,
        PEER_BETA,
        vec![
            make_photo_story(PEER_BETA, 10, now - 100, now + 3600),
            make_photo_story(PEER_BETA, 11, now - 50, now + 3600),
        ],
        0,
        now,
    );
    store.take_actions();

    // Enable main strip preloading -> starts preloading top source (PEER_BETA, newer).
    store.increment_preloading_main_sources();
    assert_eq!(
        store.current_preload(),
        Some(FullStoryId::new(PEER_BETA, 10))
    );
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::StartPreload {
            story_id: FullStoryId::new(PEER_BETA, 10),
        }]
    );

    // Viewer preload queue has higher priority; if current preload stays in top 3 it continues,
    // once pushed out of top 3 it cancels and starts the viewer item.
    store.set_preloading_in_viewer(vec![
        FullStoryId::new(PEER_BETA, 11),
        FullStoryId::new(PEER_ALPHA, 1),
        FullStoryId::new(PEER_BETA, 99), // not loaded, just occupies slot
    ]);
    // PEER_BETA:10 is now at index 3 (outside STILL_PRELOAD_FROM_FIRST=3) -> cancelled!
    let actions = store.take_actions();
    assert_eq!(
        actions,
        vec![
            StoriesAction::CancelPreload {
                story_id: FullStoryId::new(PEER_BETA, 10),
            },
            StoriesAction::StartPreload {
                story_id: FullStoryId::new(PEER_BETA, 11),
            },
        ]
    );

    // Finishing PEER_BETA:11 marks it preloaded and advances to PEER_ALPHA:1.
    store.finish_preload(FullStoryId::new(PEER_BETA, 11), true);
    assert!(store.is_preloaded(FullStoryId::new(PEER_BETA, 11)));
    assert_eq!(
        store.current_preload(),
        Some(FullStoryId::new(PEER_ALPHA, 1))
    );

    // Advancing clock past PEER_ALPHA:1 expiry deletes it (not in profile, no archive).
    store.tick(now + 60, 0);
    assert_eq!(
        store.lookup(FullStoryId::new(PEER_ALPHA, 1)),
        Err(NoStory::Deleted)
    );
    assert!(store.source(PEER_ALPHA).is_none());
}

// ===========================================================================
// 4. Viewer Auto-Advance, Pause, Navigation Edge Cases, Stealth, Reply, Areas
// ===========================================================================

#[test]
fn viewer_photo_auto_advance_pause_resume_and_mark_read() {
    let mut store = StoriesStore::new(SELF_USER);
    let now_secs = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));

    add_peer_source(
        &mut store,
        PEER_ALPHA,
        vec![
            make_photo_story(PEER_ALPHA, 1, now_secs - 20, now_secs + 3600),
            make_photo_story(PEER_ALPHA, 2, now_secs - 10, now_secs + 3600),
        ],
        0,
        now_secs,
    );

    let mut viewer = StoryViewerController::new();
    let cfg = ViewerConfig::default();
    assert!(viewer.show(
        &mut store,
        FullStoryId::new(PEER_ALPHA, 1),
        StoriesContext::SourcesList(StorySourcesList::NotHidden),
        cfg,
        now_secs,
        0,
    ));
    viewer.take_events();

    // Before ready(), photo playback does not advance.
    viewer.tick(&mut store, cfg, now_secs, 10_000);
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 1)));
    assert!(!viewer.is_viewed());

    // Signal ready at t = 10_000ms.
    viewer.ready(10_000);
    assert_eq!(viewer.pause_state(), PauseState::Playing);
    assert!(viewer.playing_allowed());

    // At +150ms: still below 200ms mark-as-read threshold.
    viewer.tick(&mut store, cfg, now_secs, 10_150);
    assert!(!viewer.is_viewed());

    // At +250ms: marked as read!
    viewer.tick(&mut store, cfg, now_secs, 10_250);
    assert!(viewer.is_viewed());
    assert_eq!(store.read_till(PEER_ALPHA), 1);

    // Pause via press-and-hold for 4000ms -> elapsed time stays frozen at 2000ms!
    viewer.content_pressed(true, 12_000);
    assert_eq!(viewer.pause_state(), PauseState::Paused);
    assert!(!viewer.playing_allowed());
    viewer.tick(&mut store, cfg, now_secs, 16_000);
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 1)));
    assert!((viewer.slider_state().active_progress - 0.4).abs() < 1e-6);

    // Release hold at t = 16_000ms; pause via menu for 1000ms; resume at 17_000ms.
    viewer.content_pressed(false, 16_000);
    viewer.set_menu_shown(true, 16_000);
    assert_eq!(viewer.pause_state(), PauseState::Inactive);
    viewer.set_menu_shown(false, 17_000);
    assert_eq!(viewer.pause_state(), PauseState::Playing);

    // 3000ms remain (out of 5000ms), so at 19_999ms still story 1, at 20_000ms auto-advances to story 2!
    viewer.tick(&mut store, cfg, now_secs, 19_999);
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 1)));
    viewer.tick(&mut store, cfg, now_secs, 20_000);
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 2)));

    // Finish story 2 -> no more stories -> emits ViewerEvent::Close!
    viewer.ready(20_000);
    viewer.take_events();
    viewer.tick(&mut store, cfg, now_secs, 25_000);
    assert!(viewer.take_events().contains(&ViewerEvent::Close));
}

#[test]
fn viewer_navigation_edge_cases_siblings_and_unread_filter() {
    let mut store = StoriesStore::new(SELF_USER);
    let now = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(true, false)); // unread #1
    store.set_peer_meta(PEER_BETA, make_contact_meta(false, false)); // unread #2
    store.set_peer_meta(PEER_GAMMA, make_contact_meta(false, false)); // already read

    add_peer_source(
        &mut store,
        PEER_ALPHA,
        vec![
            make_photo_story(PEER_ALPHA, 1, now - 30, now + 3600),
            make_photo_story(PEER_ALPHA, 2, now - 20, now + 3600),
        ],
        0,
        now,
    );
    add_peer_source(
        &mut store,
        PEER_BETA,
        vec![make_photo_story(PEER_BETA, 10, now - 10, now + 3600)],
        0,
        now,
    );
    add_peer_source(
        &mut store,
        PEER_GAMMA,
        vec![make_photo_story(PEER_GAMMA, 20, now - 5, now + 3600)],
        20, // all read!
        now,
    );

    let mut viewer = StoryViewerController::new();
    let cfg = ViewerConfig::default();
    let ctx = StoriesContext::SourcesList(StorySourcesList::NotHidden);

    // Opening unread story on PEER_ALPHA activates `showing_unread_sources`: PEER_GAMMA is excluded!
    assert!(viewer.show(
        &mut store,
        FullStoryId::new(PEER_ALPHA, 1),
        ctx,
        cfg,
        now,
        0
    ));
    assert!(viewer.showing_unread_sources());
    assert_eq!(viewer.cached_source_peers(), vec![PEER_ALPHA, PEER_BETA]);
    assert!(viewer.sibling_left().is_none());
    assert_eq!(
        viewer.sibling_right().map(|s| s.full_id()),
        Some(FullStoryId::new(PEER_BETA, 10))
    );

    // Edge case: Prev at index 0 with no left sibling restarts at index 0 and returns true!
    assert!(!viewer.subjump_available(-1));
    assert!(viewer.subjump_for(&mut store, -1, cfg, now, 100));
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 1)));

    // Next -> PEER_ALPHA story 2 -> Next -> jumps into PEER_BETA story 10!
    assert!(viewer.subjump_for(&mut store, 1, cfg, now, 200));
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 2)));
    assert!(viewer.subjump_for(&mut store, 1, cfg, now, 300));
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_BETA, 10)));
    // Left sibling remembers PEER_ALPHA's last shown story (2)!
    assert_eq!(
        viewer.sibling_left().map(|s| s.full_id()),
        Some(FullStoryId::new(PEER_ALPHA, 2))
    );
    // Right sibling is None (PEER_GAMMA was read, excluded by showing_unread_sources).
    assert!(viewer.sibling_right().is_none());
    assert!(!viewer.subjump_for(&mut store, 1, cfg, now, 400));

    // Prev at index 0 WITH left sibling jumps back to PEER_ALPHA story 2!
    assert!(viewer.subjump_available(-1));
    assert!(viewer.subjump_for(&mut store, -1, cfg, now, 500));
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 2)));
}

#[test]
fn viewer_reply_visibility_unsupported_stealth_and_areas() {
    let mut store = StoriesStore::new(SELF_USER);
    let now = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));
    store.set_peer_meta(
        CHANNEL_ONE,
        PeerStoryMeta {
            ctx: StoryPeerContext {
                is_channel: true,
                is_broadcast: true,
                has_username: true,
                ..StoryPeerContext::default()
            },
            channel_am_in: true,
            ..PeerStoryMeta::default()
        },
    );
    store.set_peer_meta(
        MEGAGROUP_ONE,
        PeerStoryMeta {
            ctx: StoryPeerContext {
                is_channel: true,
                is_megagroup: true,
                ..StoryPeerContext::default()
            },
            channel_am_in: true,
            ..PeerStoryMeta::default()
        },
    );

    // 1. Unsupported story -> CantReply, empty caption, UnsupportedStoryShown event.
    let mut unsupported = make_photo_story(PEER_ALPHA, 1, now, now + 3600);
    unsupported.media = StoryMedia::Unsupported;
    store.insert_story(unsupported, now);
    let mut viewer = StoryViewerController::new();
    viewer.show(
        &mut store,
        FullStoryId::new(PEER_ALPHA, 1),
        StoriesContext::Single,
        ViewerConfig::default(),
        now,
        0,
    );
    assert!(viewer.is_unsupported());
    assert!(viewer.caption_text().is_empty());
    assert_eq!(viewer.reply_area(), &ReplyAreaState::CantReply);
    assert!(
        viewer
            .take_events()
            .contains(&ViewerEvent::UnsupportedStoryShown(FullStoryId::new(
                PEER_ALPHA, 1
            )))
    );

    // 2. Broadcast channel -> ReplyArea Hidden, RecentViewsType::Channel.
    let ch_story = make_photo_story(CHANNEL_ONE, 2, now, now + 3600);
    store.insert_story(ch_story, now);
    viewer.show(
        &mut store,
        FullStoryId::new(CHANNEL_ONE, 2),
        StoriesContext::Single,
        ViewerConfig::default(),
        now,
        0,
    );
    assert_eq!(viewer.reply_area(), &ReplyAreaState::Hidden);
    assert_eq!(viewer.recent_views().kind, Some(RecentViewsType::Channel));

    // 3. Megagroup -> ReplyArea Visible (Comment).
    let mg_story = make_photo_story(MEGAGROUP_ONE, 3, now, now + 3600);
    store.insert_story(mg_story, now);
    viewer.show(
        &mut store,
        FullStoryId::new(MEGAGROUP_ONE, 3),
        StoriesContext::Single,
        ViewerConfig::default(),
        now,
        0,
    );
    assert!(matches!(
        viewer.reply_area(),
        ReplyAreaState::Visible {
            kind: ReplyAreaKind::Comment,
            placeholder: ReplyPlaceholder::Comment,
            ..
        }
    ));

    // 4. Stealth mode activation rules + time formatting.
    assert_eq!(time_left_text(5), "0:05");
    assert_eq!(time_left_text(125), "2:05");
    assert_eq!(time_left_text(3661), "1:01:01");
    assert_eq!(
        evaluate_stealth_mode(StealthMode::default(), now, false),
        StealthActivationDecision::RequiresPremium
    );
    assert_eq!(
        evaluate_stealth_mode(
            StealthMode {
                enabled_till: now + 300,
                cooldown_till: now + 3600,
            },
            now,
            true
        ),
        StealthActivationDecision::AlreadyActive { seconds_left: 300 }
    );
    assert_eq!(
        evaluate_stealth_mode(
            StealthMode {
                enabled_till: 0,
                cooldown_till: now + 600,
            },
            now,
            true
        ),
        StealthActivationDecision::InCooldown { seconds_left: 600 }
    );
    store.take_actions();
    assert_eq!(
        viewer.setup_stealth_mode(&mut store, now, true),
        StealthActivationDecision::ReadyToActivate
    );
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::ActivateStealthMode]
    );

    // 5. Area hit-testing & weather toggle.
    let mut area_story = make_photo_story(PEER_ALPHA, 4, now, now + 3600);
    area_story.url_areas.push(UrlArea {
        area: StoryArea::from_tl_coords(50.0, 50.0, 20.0, 20.0, 0.0, None),
        url: "https://example.test".to_string(),
    });
    area_story.weather_areas.push(WeatherArea {
        area: StoryArea::from_tl_coords(80.0, 20.0, 10.0, 10.0, 0.0, None),
        emoji: "\u{2600}".to_string(),
        color: 0,
        millicelsius: 20_000,
    });
    store.insert_story(area_story, now);
    viewer.show(
        &mut store,
        FullStoryId::new(PEER_ALPHA, 4),
        StoriesContext::Single,
        ViewerConfig::default(),
        now,
        0,
    );
    assert_eq!(
        viewer.hit_test_area(&store, 0.5, 0.5),
        Some(StoryAreaHit::Url {
            url: "https://example.test".to_string()
        })
    );
    assert!(matches!(
        viewer.hit_test_area(&store, 0.8, 0.2),
        Some(StoryAreaHit::Weather {
            millicelsius: 20_000,
            ..
        })
    ));
    assert_eq!(viewer.hit_test_area(&store, 0.05, 0.05), None);
    assert!(viewer.weather_in_celsius());
    assert!(!viewer.toggle_weather_unit());

    // 6. Caption expand/close pauses & resumes playback.
    viewer.ready(0);
    assert!(viewer.show_full_caption(100));
    assert!(viewer.is_caption_expanded());
    assert!(viewer.is_content_faded());
    assert!(!viewer.playing_allowed());
    viewer.caption_closing(200);
    assert!(!viewer.is_content_faded());
    assert!(viewer.playing_allowed());
    viewer.caption_closed(300);
    assert!(!viewer.is_caption_expanded());
}

#[test]
fn viewer_video_playback_and_album_same_day_slider() {
    let mut store = StoriesStore::new(SELF_USER);
    let day1 = 1_700_000_000;
    let day2 = day1 + 86_400;

    // Create 4 saved stories: 1 pinned (id 1), 2 on day2 (id 4, 3), 1 on day1 (id 2).
    let mut s4 = make_photo_story(SELF_USER, 4, day2 + 100, day2 + 3600);
    s4.media = StoryMedia::Video {
        document: make_video(4, 15.0),
        silent: false,
    };
    s4.in_profile = true;
    let mut s3 = make_photo_story(SELF_USER, 3, day2 + 10, day2 + 3600);
    s3.in_profile = true;
    let mut s2 = make_photo_story(SELF_USER, 2, day1 + 10, day1 + 3600);
    s2.in_profile = true;
    let mut s1 = make_photo_story(SELF_USER, 1, day1, day1 + 3600);
    s1.in_profile = true;

    store.apply_album_stories(
        SELF_USER,
        STORIES_ALBUM_ID_SAVED,
        ParsedStoriesPage {
            count: 4,
            items: vec![
                ParsedStoryItem::Full(Box::new(s4)),
                ParsedStoryItem::Full(Box::new(s3)),
                ParsedStoryItem::Full(Box::new(s2)),
                ParsedStoryItem::Full(Box::new(s1)),
            ],
            pinned_to_top: vec![1],
        },
        false,
        day2 + 200,
    );

    // Pinned story 1 comes first in respecting_pinned: [1, 4, 3, 2].
    assert_eq!(
        store
            .album_ids(SELF_USER, STORIES_ALBUM_ID_SAVED)
            .respecting_pinned(),
        vec![1, 4, 3, 2]
    );

    let mut viewer = StoryViewerController::new();
    let cfg = ViewerConfig::default();
    assert!(viewer.show(
        &mut store,
        FullStoryId::new(SELF_USER, 4),
        StoriesContext::Album(STORIES_ALBUM_ID_SAVED),
        cfg,
        day2 + 200,
        0,
    ));
    // Story 4 shares day2 with story 3 -> slider has 2 segments, active index 0.
    let slider = viewer.slider_state();
    assert_eq!(slider.total, 2);
    assert_eq!(slider.index, 0);

    // Video playback TrackState advances progress and auto-advances to story 3 at StoppedAtEnd.
    viewer.ready(0);
    viewer.update_video_playback(
        &mut store,
        TrackState {
            status: PlaybackStatus::Playing,
            position: 7_500,
            length: 15_000,
            frequency: 1_000,
        },
        cfg,
        day2 + 200,
        7_500,
    );
    assert!((viewer.slider_state().active_progress - 0.5).abs() < 1e-6);
    assert_eq!(store.viewed_self_till(), 4);

    viewer.update_video_playback(
        &mut store,
        TrackState {
            status: PlaybackStatus::StoppedAtEnd,
            position: 15_000,
            length: 15_000,
            frequency: 1_000,
        },
        cfg,
        day2 + 200,
        15_000,
    );
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(SELF_USER, 3)));
    assert_eq!(viewer.slider_state().index, 1);
}

// ===========================================================================
// 5. Dialogs Stories Top Bar & Ring Segments Tests
// ===========================================================================

#[test]
fn dialogs_bar_ring_segments_hysteresis_and_collapsed_selection() {
    let mut store = StoriesStore::new(SELF_USER);
    let now = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));
    store.set_peer_meta(PEER_BETA, make_contact_meta(false, false));

    add_peer_source(
        &mut store,
        SELF_USER,
        vec![
            make_photo_story(SELF_USER, 1, now - 100, now + 3600),
            make_photo_story(SELF_USER, 2, now - 50, now + 3600),
        ],
        0,
        now,
    );
    add_peer_source(
        &mut store,
        PEER_ALPHA,
        vec![
            make_photo_story(PEER_ALPHA, 1, now - 40, now + 3600),
            make_photo_story(PEER_ALPHA, 2, now - 30, now + 3600),
            make_photo_story(PEER_ALPHA, 3, now - 20, now + 3600),
        ],
        1, // 1 read, 2 unread
        now,
    );
    add_peer_source(
        &mut store,
        PEER_BETA,
        vec![make_photo_story(PEER_BETA, 10, now - 10, now + 3600)],
        10, // all read
        now,
    );

    // Self has story 2 > viewed_self_till (0) -> mine_unread = true!
    let content = build_dialog_stories_content(
        &store,
        StorySourcesList::NotHidden,
        |pid| match pid {
            p if p == SELF_USER => "Me".to_string(),
            p if p == PEER_ALPHA => "Alpha".to_string(),
            p if p == PEER_BETA => "Beta".to_string(),
            _ => "Other".to_string(),
        },
        &[],
    );
    assert_eq!(content.total, 3);
    assert_eq!(content.elements[0].id, SELF_USER.raw());
    assert!(content.elements[0].mine && content.elements[0].mine_unread);
    assert!(content.elements[0].skip_small);

    // Self ring: 2 segments -> [Read, Unread] because mine_unread highlights final arc!
    let self_segs = compute_ring_segments(&content.elements[0]);
    assert_eq!(
        self_segs,
        vec![
            RingSegment {
                index: 0,
                kind: RingSegmentKind::Read,
            },
            RingSegment {
                index: 1,
                kind: RingSegmentKind::Unread,
            },
        ]
    );

    // Alpha ring: 3 stories, 2 unread -> [Read, Unread, Unread].
    let alpha_segs = compute_ring_segments(&content.elements[1]);
    assert_eq!(
        alpha_segs,
        vec![
            RingSegment {
                index: 0,
                kind: RingSegmentKind::Read,
            },
            RingSegment {
                index: 1,
                kind: RingSegmentKind::Unread,
            },
            RingSegment {
                index: 2,
                kind: RingSegmentKind::Unread,
            },
        ]
    );

    // Live stream ring: single VideoStream segment regardless of count.
    let live_el = DialogStoryElement {
        count: 5,
        unread_count: 2,
        has_video_stream: true,
        ..DialogStoryElement::default()
    };
    assert_eq!(
        compute_ring_segments(&live_el),
        vec![RingSegment {
            index: 0,
            kind: RingSegmentKind::VideoStream,
        }]
    );

    // DialogStoriesBarState: collapsed skips self (index 0) and shows [1, 2].
    let mut bar = DialogStoriesBarState::new(100, 64, 10);
    bar.set_viewport_width(120);
    bar.set_content(content);
    assert_eq!(bar.small_visible_indices(), vec![1, 2]);
    assert_eq!(
        bar.tooltip_summary(),
        TooltipSummary::Peers(vec!["Alpha".to_string(), "Beta".to_string()])
    );

    // Expand/collapse hysteresis: 0.70 doesn't expand, 0.75 expands, 0.70 stays expanded, 0.65 collapses.
    assert_eq!(bar.set_expanded_height(70), None);
    assert!(!bar.is_expanded());
    assert_eq!(bar.set_expanded_height(75), Some(true));
    assert!(bar.is_expanded());
    assert_eq!(bar.set_expanded_height(70), None);
    assert!(bar.is_expanded());
    assert_eq!(bar.set_expanded_height(65), Some(false));
    assert!(!bar.is_expanded());

    // Build last for peer (up to 3 newest stories, newest first).
    let last_alpha = build_last_for_peer(&store, PEER_ALPHA);
    assert_eq!(last_alpha.total, 3);
    assert_eq!(
        last_alpha
            .elements
            .iter()
            .map(|e| (e.id, e.unread_count))
            .collect::<Vec<_>>(),
        vec![(3, 1), (2, 1), (1, 0)]
    );
}

#[test]
fn album_ids_window_and_pin_limits() {
    let ids = StoriesIds {
        list: vec![10, 9, 8, 7, 6, 5],
        pinned_to_top: vec![5, 7],
    };
    assert_eq!(respecting_pinned(&ids), vec![5, 7, 10, 9, 8, 6]);

    let (slice, load_more) = StoriesIdsSlice::build_album_window(&ids, 20, 9, 2);
    // sorted = [5, 7, 10, 9, 8, 6], index of 9 is 3; takeBefore=2 (indices 1..3 -> 7,10), takeAfter=2 (indices 3..5 -> 9,8)
    assert_eq!(slice.ids, vec![7, 10, 9, 8]);
    assert_eq!(slice.skipped_before, Some(1));
    assert_eq!(slice.skipped_after, Some(15));
    assert!(!load_more); // has_after = 3 >= limit 2

    // Pin limit enforcement (default max 3).
    let mut store = StoriesStore::new(SELF_USER);
    let now = 1_700_000_000;
    store.apply_album_stories(
        SELF_USER,
        STORIES_ALBUM_ID_SAVED,
        ParsedStoriesPage {
            count: 4,
            items: (1..=4)
                .map(|id| {
                    let mut s = make_photo_story(SELF_USER, id, now, now + 3600);
                    s.in_profile = true;
                    ParsedStoryItem::Full(Box::new(s))
                })
                .collect(),
            pinned_to_top: vec![1, 2],
        },
        false,
        now,
    );
    assert!(store.can_toggle_pinned_list(&[FullStoryId::new(SELF_USER, 3)], true));
    assert!(!store.can_toggle_pinned_list(
        &[
            FullStoryId::new(SELF_USER, 3),
            FullStoryId::new(SELF_USER, 4),
        ],
        true
    ));
    assert!(store.can_toggle_pinned_list(&[FullStoryId::new(SELF_USER, 1)], false));

    // Convert tg_data caption to tg_text.
    let caption = TextWithEntities::new(
        "Hello bold".to_string(),
        vec![TextEntity::new(6, 4, EntityKind::Bold)],
    );
    let rich = to_tg_text(&caption);
    assert_eq!(rich.text, "Hello bold");
    assert_eq!(rich.entities.len(), 1);
    assert_eq!(rich.entities[0].entity_type(), tg_text::EntityType::Bold);
}

#[test]
fn tl_views_list_reactions_list_and_albums() {
    let now = 1_700_000_000;
    let views_tl =
        tl_enums::stories::StoryViewsList::StoryViewsList(tl_types::stories::StoryViewsList {
            count: 3,
            views_count: 2,
            forwards_count: 1,
            reactions_count: 1,
            views: vec![
                tl_enums::StoryView::StoryView(tl_types::StoryView {
                    blocked: true,
                    blocked_my_stories_from: false,
                    user_id: 201,
                    date: now - 10,
                    reaction: Some(tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "\u{1F44D}".to_string(),
                    })),
                }),
                tl_enums::StoryView::PublicRepost(tl_types::StoryViewPublicRepost {
                    blocked: false,
                    blocked_my_stories_from: true,
                    peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 202 }),
                    story: tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
                        pinned: false,
                        public: true,
                        close_friends: false,
                        min: false,
                        noforwards: false,
                        edited: false,
                        contacts: false,
                        selected_contacts: false,
                        out: false,
                        id: 77,
                        date: now - 5,
                        from_id: None,
                        fwd_from: None,
                        expire_date: now + 3600,
                        caption: None,
                        entities: None,
                        media: Box::new(tl_photo_media(77)),
                        media_areas: None,
                        privacy: None,
                        views: None,
                        sent_reaction: None,
                        albums: None,
                        music: None,
                    }),
                }),
                tl_enums::StoryView::PublicForward(tl_types::StoryViewPublicForward {
                    blocked: false,
                    blocked_my_stories_from: false,
                    message: tl_enums::Message::Message(tl_types::Message {
                        out: false,
                        mentioned: false,
                        media_unread: false,
                        silent: false,
                        post: true,
                        from_scheduled: false,
                        legacy: false,
                        edit_hide: false,
                        pinned: false,
                        noforwards: false,
                        invert_media: false,
                        offline: false,
                        video_processing_pending: false,
                        paid_suggested_post_stars: false,
                        paid_suggested_post_ton: false,
                        id: 900,
                        from_id: None,
                        from_boosts_applied: None,
                        from_rank: None,
                        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: 5001,
                        }),
                        saved_peer_id: None,
                        fwd_from: None,
                        via_bot_id: None,
                        via_business_bot_id: None,
                        guestchat_via_from: None,
                        reply_to: None,
                        date: now - 2,
                        message: String::new(),
                        media: None,
                        reply_markup: None,
                        entities: None,
                        views: None,
                        forwards: None,
                        replies: None,
                        edit_date: None,
                        post_author: None,
                        grouped_id: None,
                        reactions: None,
                        restriction_reason: None,
                        ttl_period: None,
                        quick_reply_shortcut_id: None,
                        effect: None,
                        factcheck: None,
                        report_delivery_until_date: None,
                        paid_message_stars: None,
                        suggested_post: None,
                        schedule_repeat_period: None,
                        summary_from_language: None,
                        rich_message: None,
                    }),
                }),
            ],
            chats: Vec::new(),
            users: Vec::new(),
            next_offset: Some("off_1".to_string()),
        });
    let parsed = story_views_list_from_tl(&views_tl, now, |_| StoryPeerContext::default());
    assert_eq!(parsed.slice.list.len(), 3);
    assert_eq!(parsed.slice.list[0].kind(), StoryViewKind::Viewed);
    assert!(parsed.slice.list[0].blocked);
    assert_eq!(parsed.slice.list[1].kind(), StoryViewKind::Reposted);
    assert_eq!(parsed.slice.list[1].repost_id, 77);
    assert_eq!(parsed.slice.list[2].kind(), StoryViewKind::Forwarded);
    assert_eq!(parsed.slice.list[2].forward_id, MsgId::from(900));
    assert_eq!(parsed.repost_stories.len(), 1);
    assert_eq!(parsed.slice.next_offset, "off_1");

    // Albums parsing & NotModified.
    let albums_tl = tl_enums::stories::Albums::Albums(tl_types::stories::Albums {
        hash: 12345,
        albums: vec![tl_enums::StoryAlbum::StoryAlbum(tl_types::StoryAlbum {
            album_id: 3,
            title: "Trips".to_string(),
            icon_photo: None,
            icon_video: None,
        })],
    });
    let mut store = StoriesStore::new(SELF_USER);
    store.apply_tl_albums(SELF_USER, &albums_tl);
    assert_eq!(store.albums_list(SELF_USER).len(), 1);
    assert_eq!(store.albums_list(SELF_USER)[0].title, "Trips");
    store.apply_tl_albums(SELF_USER, &tl_enums::stories::Albums::NotModified);
    assert_eq!(store.albums_list(SELF_USER).len(), 1);
}

#[test]
fn viewer_waiting_resolve_actions_polling_and_uploads() {
    let mut store = StoriesStore::new(SELF_USER);
    let now = 1_700_000_000;
    store.set_peer_meta(PEER_ALPHA, make_contact_meta(false, false));

    // Peer Alpha has story 1 loaded and story 2 skipped (unresolved).
    store.apply_peer_stories(
        ParsedPeerStories {
            peer_id: PEER_ALPHA,
            max_read_id: 0,
            items: vec![
                ParsedStoryItem::Full(Box::new(make_photo_story(
                    PEER_ALPHA,
                    1,
                    now - 20,
                    now + 3600,
                ))),
                ParsedStoryItem::Skipped(StoryIdDates::new(2, now - 10, now + 3600, false)),
            ],
            has_video_stream: false,
        },
        ParseSource::MyStrip,
        now,
    );
    store.take_actions();

    let mut viewer = StoryViewerController::new();
    let cfg = ViewerConfig::default();
    assert!(viewer.show(
        &mut store,
        FullStoryId::new(PEER_ALPHA, 1),
        StoriesContext::Peer,
        cfg,
        now,
        0,
    ));

    // Next story (2) is not resolved yet -> sets waiting_for_id and issues GetStoriesById.
    assert!(viewer.subjump_for(&mut store, 1, cfg, now, 500));
    assert_eq!(
        viewer.waiting_for_id(),
        Some(FullStoryId::new(PEER_ALPHA, 2))
    );

    // Resolve story 2 -> on_items_changed completes the jump automatically!
    store.apply_resolved_stories(
        PEER_ALPHA,
        vec![ParsedStoryItem::Full(Box::new(make_photo_story(
            PEER_ALPHA,
            2,
            now - 10,
            now + 3600,
        )))],
        now,
    );
    viewer.on_items_changed(&mut store, PEER_ALPHA, cfg, now, 600);
    assert_eq!(viewer.waiting_for_id(), None);
    assert_eq!(viewer.shown_id(), Some(FullStoryId::new(PEER_ALPHA, 2)));

    // Share capability for public story on peer with username.
    assert_eq!(
        viewer.share_requested(&store, now),
        ShareCapability::ShareStory {
            story_id: FullStoryId::new(PEER_ALPHA, 2),
            can_copy_link: true,
        }
    );

    // Report is allowed for non-self peer.
    assert!(viewer.report_requested(&store));

    // Story destroyed closes viewer.
    viewer.take_events();
    viewer.on_story_destroyed(FullStoryId::new(PEER_ALPHA, 2));
    assert!(viewer.take_events().contains(&ViewerEvent::Close));

    // Viewer polling fires after 60s.
    store.take_actions();
    store.tick(now + POLLING_INTERVAL_VIEWER_SECS, 60_000);
    assert!(store.take_actions().iter().any(|a| matches!(
        a,
        StoriesAction::GetStoriesById { peer_id, ids } if *peer_id == PEER_ALPHA && ids.contains(&2)
    )));

    // Dialogs bar upload status merging + ring clamping to 180 segments.
    let uploads = vec![StoryUploadStatus {
        peer_id: SELF_USER,
        peer_name: "Me".to_string(),
        progress: 0.42,
        failed: false,
    }];
    let content = build_dialog_stories_content(
        &store,
        StorySourcesList::NotHidden,
        |_| "Alpha".to_string(),
        &uploads,
    );
    assert_eq!(content.elements[0].id, SELF_USER.raw());
    assert!(content.elements[0].uploading);
    assert_eq!(content.elements[0].upload_progress, 42);
    assert!(content.elements[0].mine_unread);

    let huge_el = DialogStoryElement {
        count: 500,
        unread_count: 500,
        ..DialogStoryElement::default()
    };
    assert_eq!(
        compute_ring_segments(&huge_el).len(),
        MAX_SEGMENTS_COUNT as usize
    );
}
