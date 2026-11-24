//! Protocol conformance tests for `tg-stories` (Layer 233, tdesktop reference `c2d0ad9`).
//!
//! Covers:
//! - `stories.getAllStories` (`next`, `hidden`, `state` continuation, `allStoriesNotModified`)
//! - `stories.getPeerStories` and `stories.getStoriesByID` batching / failure recovery
//! - `min: true` `StoryItem` preservation (and no `forwards` double-counting)
//! - `stories.getPinnedStories` / `stories.getStoriesArchive` / `stories.getAlbumStories` paging
//! - `stories.readStories` (`max_id` debounce/coalescing) & `stories.incrementStoryViews` batching
//! - `stories.getStoryViewsList` (`just_contacts`, `reactions_first`, `forwards_first`, `q`, `offset`)
//! - `stories.sendReaction`, `stories.togglePinned`, `stories.togglePinnedToTop`, `stories.deleteStories`
//! - `stories.sendStory` & `stories.editStory` request shapes (privacy rules, period, media areas, repost)
//! - `updateStory`, `updateReadStories`, `updateStoriesStealthMode`, `updateStoryID`, `activateStealthMode`
//! - Layer 233 story albums (`getAlbums`, `getAlbumStories`, `createAlbum`, `updateAlbum`, `reorderAlbums`, `deleteAlbum`)

use tg_data::peer::GeoPoint;
use tg_data::{
    ChannelId, DocumentId, FullMsgId, FullStoryId, MsgId, PeerId, ReactionId, StoryId, TimeId,
    UserId,
};
use tg_stories::story::StoryLocation;
use tg_stories::{
    ARCHIVE_FIRST_PER_PAGE, ARCHIVE_PER_PAGE, ChannelPost, EditStoryInput,
    INCREMENT_VIEWS_DELAY_MS, MARK_AS_READ_DELAY_MS, MAX_RESOLVE_TOGETHER, NoStory, ParseSource,
    ParsedAllStories, ParsedStoryItem, ParsedStoryUpdate, RectF, SAVED_FIRST_PER_PAGE,
    SAVED_PER_PAGE, STORIES_ALBUM_ID_ARCHIVE, STORIES_ALBUM_ID_SAVED, SendStoryInput, StealthMode,
    StoriesAction, StoriesEvent, StoriesStore, StoryArea, StoryMediaAreasInput, StoryPeerContext,
    StoryPrivacyOption, StoryPrivacyRuleInput, StoryRepostInput, StorySourcesList,
    StoryViewsListFilter, SuggestedReaction, UrlArea, WeatherArea, build_activate_stealth_mode,
    build_can_send_story, build_create_album, build_delete_album, build_delete_stories,
    build_edit_story, build_get_album_stories, build_get_albums, build_get_all_stories,
    build_get_chats_to_send, build_get_peer_stories, build_get_pinned_stories,
    build_get_stories_archive, build_get_stories_by_id, build_get_story_views_list,
    build_get_story_views_list_filtered, build_increment_story_views, build_read_stories,
    build_rename_album, build_reorder_album, build_reorder_albums, build_send_reaction,
    build_send_story, build_toggle_pinned, build_toggle_pinned_to_top, build_update_album_stories,
    evaluate_stealth_mode, media_areas_to_tl, story_privacy_to_tl, story_views_list_from_tl,
};
use tg_tl::{
    Deserializable, Serializable, enums as tl_enums, functions as tl_fn, types as tl_types,
};

const SELF_USER: UserId = UserId::from_tl(1000);
const SELF_PEER: PeerId = PeerId::from_user(SELF_USER);
const PEER_USER_A: UserId = UserId::from_tl(2001);
const PEER_A: PeerId = PeerId::from_user(PEER_USER_A);
const PEER_USER_B: UserId = UserId::from_tl(2002);
const PEER_B: PeerId = PeerId::from_user(PEER_USER_B);
const CHANNEL_ID: ChannelId = ChannelId::from_tl(3001);
const CHANNEL_PEER: PeerId = PeerId::from_channel(CHANNEL_ID);
const NOW: TimeId = 1_750_000_000;

fn input_peer_user(uid: UserId, hash: i64) -> tl_enums::InputPeer {
    tl_enums::InputPeer::User(tl_types::InputPeerUser {
        user_id: uid.to_tl(),
        access_hash: hash,
    })
}

fn input_user(uid: UserId, hash: i64) -> tl_enums::InputUser {
    tl_enums::InputUser::InputUser(tl_types::InputUser {
        user_id: uid.to_tl(),
        access_hash: hash,
    })
}

fn tl_photo_media(photo_id: i64) -> Box<tl_enums::MessageMedia> {
    Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
            has_stickers: false,
            id: photo_id,
            access_hash: photo_id * 17,
            file_reference: vec![1, 2, 3, 4],
            date: NOW - 60,
            sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "x".to_owned(),
                w: 720,
                h: 1280,
                size: 42_000,
            })],
            video_sizes: None,
            dc_id: 2,
        })),
        ttl_seconds: None,
        video: None,
    }))
}

#[allow(clippy::too_many_arguments)]
fn tl_story_item(
    id: StoryId,
    min: bool,
    out: bool,
    pinned: bool,
    sent_reaction: Option<tl_enums::Reaction>,
    views: Option<tl_enums::StoryViews>,
    privacy: Option<Vec<tl_enums::PrivacyRule>>,
    caption: Option<&str>,
) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned,
        public: true,
        close_friends: false,
        min,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out,
        id,
        date: NOW - 120,
        from_id: None,
        fwd_from: None,
        expire_date: NOW + 86_400,
        caption: caption.map(str::to_owned),
        entities: None,
        media: tl_photo_media(9000 + i64::from(id)),
        media_areas: None,
        privacy,
        views,
        sent_reaction,
        albums: None,
        music: None,
    })
}

#[test]
fn get_all_stories_wire_flags_and_state_continuation() {
    let first = build_get_all_stories(false, None);
    assert!(!first.next);
    assert!(!first.hidden);
    assert!(first.state.is_none());
    let wire = first.to_bytes();
    let decoded = tl_fn::stories::GetAllStories::from_bytes(&wire).unwrap();
    assert_eq!(decoded, first);

    let cont_hidden = build_get_all_stories(true, Some("state_v2_abc"));
    assert!(cont_hidden.next);
    assert!(cont_hidden.hidden);
    assert_eq!(cont_hidden.state.as_deref(), Some("state_v2_abc"));
    let wire2 = cont_hidden.to_bytes();
    let decoded2 = tl_fn::stories::GetAllStories::from_bytes(&wire2).unwrap();
    assert_eq!(decoded2, cont_hidden);

    // Empty state string must clear `next` and omit `state` flag.
    let empty_state = build_get_all_stories(false, Some(""));
    assert!(!empty_state.next);
    assert!(empty_state.state.is_none());

    // Store pagination + NotModified + stealth mode propagation.
    let mut store = StoriesStore::new(SELF_PEER);
    store.load_more(StorySourcesList::NotHidden);
    let actions = store.take_actions();
    assert_eq!(
        actions,
        vec![StoriesAction::GetAllStories {
            list: StorySourcesList::NotHidden,
            state: None,
        }]
    );

    let page1 = tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
        has_more: true,
        count: 2,
        state: "tok_1".to_owned(),
        peer_stories: vec![tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
            peer: tl_enums::Peer::User(tl_types::PeerUser {
                user_id: PEER_USER_A.to_tl(),
            }),
            max_read_id: Some(10),
            stories: vec![tl_story_item(
                11,
                false,
                false,
                false,
                None,
                None,
                None,
                Some("hello"),
            )],
        })],
        chats: vec![],
        users: vec![],
        stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
            tl_types::StoriesStealthMode {
                active_until_date: Some(NOW + 900),
                cooldown_until_date: Some(NOW + 3600),
            },
        ),
    });
    store.apply_tl_all_stories(StorySourcesList::NotHidden, &page1, NOW);
    assert!(!store.sources_loaded(StorySourcesList::NotHidden));
    assert_eq!(
        store.stealth_mode(),
        StealthMode {
            enabled_till: NOW + 900,
            cooldown_till: NOW + 3600,
        }
    );

    // Next page uses returned state token.
    store.load_more(StorySourcesList::NotHidden);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::GetAllStories {
            list: StorySourcesList::NotHidden,
            state: Some("tok_1".to_owned()),
        }]
    );

    // Failure unlocks retry without losing state token.
    store.fail_load_more(StorySourcesList::NotHidden);
    store.load_more(StorySourcesList::NotHidden);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::GetAllStories {
            list: StorySourcesList::NotHidden,
            state: Some("tok_1".to_owned()),
        }]
    );

    // NotModified updates state & stealth_mode without wiping loaded sources.
    let nm = tl_enums::stories::AllStories::NotModified(tl_types::stories::AllStoriesNotModified {
        state: "tok_2".to_owned(),
        stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
            tl_types::StoriesStealthMode {
                active_until_date: Some(NOW + 1200),
                cooldown_until_date: Some(NOW + 7200),
            },
        ),
    });
    store.apply_tl_all_stories(StorySourcesList::NotHidden, &nm, NOW);
    assert_eq!(
        store.stealth_mode(),
        StealthMode {
            enabled_till: NOW + 1200,
            cooldown_till: NOW + 7200,
        }
    );
    assert!(store.lookup(FullStoryId::new(PEER_A, 11)).is_ok());

    // ParsedAllStories enum shape check.
    let parsed_nm = tg_stories::all_stories_from_tl(&nm, NOW, |_| StoryPeerContext::default());
    assert!(matches!(
        parsed_nm,
        ParsedAllStories::NotModified { ref state, .. } if state == "tok_2"
    ));
}

#[test]
fn min_story_item_preserves_existing_fields_and_does_not_double_count_forwards() {
    let mut store = StoriesStore::new(SELF_PEER);
    let initial_views = tl_enums::StoryViews::StoryViews(tl_types::StoryViews {
        has_viewers: true,
        views_count: 100,
        forwards_count: Some(7),
        reactions: Some(vec![tl_enums::ReactionCount::ReactionCount(
            tl_types::ReactionCount {
                chosen_order: Some(0),
                reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "\u{2764}".to_owned(),
                }),
                count: 5,
            },
        )]),
        reactions_count: Some(5),
        recent_viewers: Some(vec![PEER_USER_B.to_tl()]),
    });
    let full = tl_story_item(
        42,
        false,
        true,
        true,
        Some(tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: "\u{2764}".to_owned(),
        })),
        Some(initial_views),
        Some(vec![tl_enums::PrivacyRule::PrivacyValueDisallowUsers(
            tl_types::PrivacyValueDisallowUsers {
                users: vec![PEER_USER_B.to_tl()],
            },
        )]),
        Some("original"),
    );
    let ps_full = tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
        peer: tl_enums::Peer::User(tl_types::PeerUser {
            user_id: SELF_USER.to_tl(),
        }),
        max_read_id: Some(42),
        stories: vec![full],
    });
    store.apply_tl_peer_stories(&ps_full, ParseSource::DirectRequest, NOW);

    let before = store
        .lookup(FullStoryId::new(SELF_PEER, 42))
        .unwrap()
        .clone();
    assert!(before.out);
    assert_eq!(
        before.sent_reaction,
        Some(ReactionId::Emoji("\u{2764}".to_owned()))
    );
    assert_eq!(before.privacy_disallowed, vec![PEER_B]);
    assert_eq!(before.views.views, 100);
    assert_eq!(before.views.forwards, 7);
    assert_eq!(before.views.total, 107);

    // Now arrive a `min: true` update with `out: false`, `sent_reaction: None`,
    // `privacy: None`, `views: None` (typical server broadcast in `updateStory`).
    let min_update = tl_enums::Update::Story(tl_types::UpdateStory {
        peer: tl_enums::Peer::User(tl_types::PeerUser {
            user_id: SELF_USER.to_tl(),
        }),
        story: tl_story_item(42, true, false, true, None, None, None, Some("edited")),
    });
    assert!(store.apply_tl_update(&min_update, NOW + 5));

    let after = store.lookup(FullStoryId::new(SELF_PEER, 42)).unwrap();
    assert_eq!(after.caption.text, "edited");
    assert!(after.out, "min update must preserve `out`");
    assert_eq!(
        after.sent_reaction,
        Some(ReactionId::Emoji("\u{2764}".to_owned())),
        "min update must preserve `sent_reaction`"
    );
    assert_eq!(
        after.privacy_disallowed,
        vec![PEER_B],
        "min update must preserve `privacy_disallowed`"
    );
    assert_eq!(
        after.views.views, 100,
        "min update without views must NOT inflate `views` by `forwards`"
    );
    assert_eq!(after.views.forwards, 7);
    assert_eq!(
        after.views.total, 107,
        "min update without views must NOT double-count `forwards` in `total`"
    );
}

#[test]
fn resolve_stories_batching_and_failure_recovery() {
    let mut store = StoriesStore::new(SELF_PEER);
    // Queue 120 stories for PEER_A and flush -> first batch is capped at MAX_RESOLVE_TOGETHER (100).
    for sid in 1..=120 {
        store.resolve(FullStoryId::new(PEER_A, sid), false);
    }
    store.flush_resolve_requests();
    let actions = store.take_actions();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        StoriesAction::GetStoriesById { peer_id, ids } => {
            assert_eq!(*peer_id, PEER_A);
            assert_eq!(ids.len(), MAX_RESOLVE_TOGETHER);
        }
        other => panic!("unexpected action: {other:?}"),
    }

    // Simulate RPC failure on first batch -> must un-wedge and immediately flush remaining 20!
    store.fail_resolve_stories(PEER_A);
    assert_eq!(
        store.lookup(FullStoryId::new(PEER_A, 1)),
        Err(NoStory::Deleted)
    );
    let events = store.take_events();
    assert!(events.contains(&StoriesEvent::ItemsChanged(PEER_A)));
    let next_actions = store.take_actions();
    assert_eq!(next_actions.len(), 1);
    match &next_actions[0] {
        StoriesAction::GetStoriesById { peer_id, ids } => {
            assert_eq!(*peer_id, PEER_A);
            assert_eq!(ids.len(), 20);
        }
        other => panic!("unexpected follow-up action: {other:?}"),
    }

    // Apply TL response with one resolved item; missing 19 become Deleted.
    let resp = tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
        count: 1,
        stories: vec![tl_story_item(
            101, false, false, false, None, None, None, None,
        )],
        pinned_to_top: None,
        chats: vec![],
        users: vec![],
    });
    store.apply_tl_resolved_stories(PEER_A, &resp, NOW);
    assert!(store.lookup(FullStoryId::new(PEER_A, 101)).is_ok());
    assert_eq!(
        store.lookup(FullStoryId::new(PEER_A, 102)),
        Err(NoStory::Deleted)
    );
}

#[test]
fn pinned_archive_and_custom_album_paging_and_failure_handlers() {
    let mut store = StoriesStore::new(SELF_PEER);
    // Saved (pinned) stories first page = SAVED_FIRST_PER_PAGE (10), offset = 0.
    store.album_ids_load_more(SELF_PEER, STORIES_ALBUM_ID_SAVED);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::LoadAlbumStories {
            peer_id: SELF_PEER,
            album_id: STORIES_ALBUM_ID_SAVED,
            offset: 0,
            limit: SAVED_FIRST_PER_PAGE,
        }]
    );
    let page1 = tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
        count: 25,
        stories: vec![
            tl_story_item(50, false, true, true, None, None, None, None),
            tl_story_item(40, false, true, true, None, None, None, None),
        ],
        pinned_to_top: Some(vec![50]),
        chats: vec![],
        users: vec![],
    });
    store.apply_tl_album_stories(SELF_PEER, STORIES_ALBUM_ID_SAVED, &page1, false, NOW);
    assert_eq!(
        store.album_ids(SELF_PEER, STORIES_ALBUM_ID_SAVED).list,
        vec![50, 40]
    );
    assert_eq!(
        store
            .album_ids(SELF_PEER, STORIES_ALBUM_ID_SAVED)
            .pinned_to_top,
        vec![50]
    );

    // Second page uses last_id = 40 and limit = SAVED_PER_PAGE (30).
    store.album_ids_load_more(SELF_PEER, STORIES_ALBUM_ID_SAVED);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::LoadAlbumStories {
            peer_id: SELF_PEER,
            album_id: STORIES_ALBUM_ID_SAVED,
            offset: 40,
            limit: SAVED_PER_PAGE,
        }]
    );

    // Failure on second page clears in-flight, marks loaded, sets total = list.len(), and emits event.
    store.fail_album_stories(SELF_PEER, STORIES_ALBUM_ID_SAVED);
    assert_eq!(store.album_ids_count(SELF_PEER, STORIES_ALBUM_ID_SAVED), 2);
    // Reload resets loaded/last_id and requests from offset 0 again.
    store.album_ids_reload(SELF_PEER, STORIES_ALBUM_ID_SAVED);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::LoadAlbumStories {
            peer_id: SELF_PEER,
            album_id: STORIES_ALBUM_ID_SAVED,
            offset: 0,
            limit: SAVED_FIRST_PER_PAGE,
        }]
    );

    // Archive paging uses ARCHIVE_FIRST_PER_PAGE / ARCHIVE_PER_PAGE for self peer.
    store.album_ids_load_more(SELF_PEER, STORIES_ALBUM_ID_ARCHIVE);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::LoadAlbumStories {
            peer_id: SELF_PEER,
            album_id: STORIES_ALBUM_ID_ARCHIVE,
            offset: 0,
            limit: ARCHIVE_FIRST_PER_PAGE,
        }]
    );
    let arch_page1 = tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
        count: 15,
        stories: vec![tl_story_item(
            30, false, true, false, None, None, None, None,
        )],
        pinned_to_top: None,
        chats: vec![],
        users: vec![],
    });
    store.apply_tl_album_stories(SELF_PEER, STORIES_ALBUM_ID_ARCHIVE, &arch_page1, false, NOW);
    store.album_ids_load_more(SELF_PEER, STORIES_ALBUM_ID_ARCHIVE);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::LoadAlbumStories {
            peer_id: SELF_PEER,
            album_id: STORIES_ALBUM_ID_ARCHIVE,
            offset: 30,
            limit: ARCHIVE_PER_PAGE,
        }]
    );

    // Custom albums: getAlbums hash & failure reset.
    store.request_albums(SELF_PEER);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::GetAlbums {
            peer_id: SELF_PEER,
            hash: 0,
        }]
    );
    let albums_resp = tl_enums::stories::Albums::Albums(tl_types::stories::Albums {
        hash: 0x1234_5678,
        albums: vec![tl_enums::StoryAlbum::StoryAlbum(tl_types::StoryAlbum {
            album_id: 7,
            title: "Trips".to_owned(),
            icon_photo: None,
            icon_video: None,
        })],
    });
    store.apply_tl_albums(SELF_PEER, &albums_resp);
    assert_eq!(store.albums_list(SELF_PEER).len(), 1);
    store.request_albums(SELF_PEER);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::GetAlbums {
            peer_id: SELF_PEER,
            hash: 0x1234_5678,
        }]
    );
    store.fail_albums(SELF_PEER);
    store.request_albums(SELF_PEER);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::GetAlbums {
            peer_id: SELF_PEER,
            hash: 0,
        }]
    );
}

#[test]
fn read_stories_and_increment_views_batching_and_views_list_filters() {
    let mut store = StoriesStore::new(SELF_PEER);
    let ps = tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
        peer: tl_enums::Peer::User(tl_types::PeerUser {
            user_id: PEER_USER_A.to_tl(),
        }),
        max_read_id: Some(100),
        stories: vec![
            tl_story_item(102, false, false, false, None, None, None, None),
            tl_story_item(103, false, false, false, None, None, None, None),
            tl_story_item(105, false, false, false, None, None, None, None),
        ],
    });
    store.apply_tl_peer_stories(&ps, ParseSource::DirectRequest, NOW);
    store.take_actions();

    // Mark active stories 102, 105, 103 read within 3s debounce -> coalesces into single max_id = 105.
    store.mark_as_read(FullStoryId::new(PEER_A, 102), NOW, 1_000);
    store.mark_as_read(FullStoryId::new(PEER_A, 105), NOW, 1_500);
    store.mark_as_read(FullStoryId::new(PEER_A, 103), NOW, 2_000);
    assert!(store.take_actions().is_empty());

    // Mark older expired in-profile stories 20 & 25 read -> queues `incrementStoryViews` with 5s batch timer.
    let expired_page = tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
        count: 2,
        stories: vec![
            tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
                pinned: true,
                public: true,
                close_friends: false,
                min: false,
                noforwards: false,
                edited: false,
                contacts: false,
                selected_contacts: false,
                out: false,
                id: 20,
                date: NOW - 200_000,
                from_id: None,
                fwd_from: None,
                expire_date: NOW - 100_000,
                caption: None,
                entities: None,
                media: tl_photo_media(9020),
                media_areas: None,
                privacy: None,
                views: None,
                sent_reaction: None,
                albums: None,
                music: None,
            }),
            tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
                pinned: true,
                public: true,
                close_friends: false,
                min: false,
                noforwards: false,
                edited: false,
                contacts: false,
                selected_contacts: false,
                out: false,
                id: 25,
                date: NOW - 200_000,
                from_id: None,
                fwd_from: None,
                expire_date: NOW - 100_000,
                caption: None,
                entities: None,
                media: tl_photo_media(9025),
                media_areas: None,
                privacy: None,
                views: None,
                sent_reaction: None,
                albums: None,
                music: None,
            }),
        ],
        pinned_to_top: None,
        chats: vec![],
        users: vec![],
    });
    store.apply_tl_album_stories(PEER_A, STORIES_ALBUM_ID_SAVED, &expired_page, false, NOW);
    store.take_actions();
    store.mark_as_read(FullStoryId::new(PEER_A, 20), NOW, 2_000);
    store.mark_as_read(FullStoryId::new(PEER_A, 25), NOW, 2_200);
    store.mark_as_read(FullStoryId::new(PEER_A, 20), NOW, 2_300);

    // At 1_500 + MARK_AS_READ_DELAY_MS (4_500 ms), readStories flushes with max_id = 105.
    store.tick(NOW, 1_500 + MARK_AS_READ_DELAY_MS);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::ReadStories {
            peer_id: PEER_A,
            max_id: 105,
        }]
    );

    // At 2_000 + INCREMENT_VIEWS_DELAY_MS (7_000 ms), incrementStoryViews flushes deduplicated [20, 25].
    store.tick(NOW, 2_000 + INCREMENT_VIEWS_DELAY_MS);
    assert_eq!(
        store.take_actions(),
        vec![StoriesAction::IncrementStoryViews {
            peer_id: PEER_A,
            ids: vec![20, 25],
        }]
    );

    // Filtered getStoryViewsList wire verification (just_contacts, reactions_first, forwards_first, q, offset).
    let req = build_get_story_views_list_filtered(
        input_peer_user(SELF_USER, 1),
        99,
        StoryViewsListFilter {
            just_contacts: true,
            reactions_first: false,
            forwards_first: true,
            query: Some("alex".to_owned()),
        },
        "cursor_page_2".to_owned(),
        50,
    );
    let wire = req.to_bytes();
    let decoded = tl_fn::stories::GetStoryViewsList::from_bytes(&wire).unwrap();
    assert!(decoded.just_contacts);
    assert!(!decoded.reactions_first);
    assert!(decoded.forwards_first);
    assert_eq!(decoded.q.as_deref(), Some("alex"));
    assert_eq!(decoded.offset, "cursor_page_2");
    assert_eq!(decoded.limit, 50);

    // Default helper matches tdesktop (`reactions_first: true`, others false).
    let def = build_get_story_views_list(input_peer_user(SELF_USER, 1), 99, String::new(), 50);
    assert!(def.reactions_first);
    assert!(!def.just_contacts);
    assert!(!def.forwards_first);
    assert!(def.q.is_none());

    // Views list parser handles StoryView, PublicRepost, PublicForward + next_offset.
    let tl_list =
        tl_enums::stories::StoryViewsList::StoryViewsList(tl_types::stories::StoryViewsList {
            count: 3,
            views_count: 10,
            forwards_count: 2,
            reactions_count: 1,
            views: vec![
                tl_enums::StoryView::StoryView(tl_types::StoryView {
                    blocked: false,
                    blocked_my_stories_from: true,
                    user_id: PEER_USER_A.to_tl(),
                    date: NOW - 10,
                    reaction: Some(tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "\u{1F525}".to_owned(),
                    })),
                }),
                tl_enums::StoryView::PublicRepost(tl_types::StoryViewPublicRepost {
                    blocked: false,
                    blocked_my_stories_from: false,
                    peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: CHANNEL_ID.to_tl(),
                    }),
                    story: tl_story_item(77, false, false, false, None, None, None, None),
                }),
            ],
            chats: vec![],
            users: vec![],
            next_offset: Some("next_tok_3".to_owned()),
        });
    let parsed = story_views_list_from_tl(&tl_list, NOW, |_| StoryPeerContext::default());
    assert_eq!(parsed.slice.next_offset, "next_tok_3");
    assert_eq!(parsed.slice.list.len(), 2);
    assert_eq!(parsed.slice.list[0].peer_id, PEER_A);
    assert!(parsed.slice.list[0].blocked_my_stories_from);
    assert_eq!(parsed.slice.list[1].peer_id, CHANNEL_PEER);
    assert_eq!(parsed.slice.list[1].repost_id, 77);
    assert_eq!(parsed.repost_stories.len(), 1);

    // Views failure un-wedges polling.
    store.load_views_slice(SELF_PEER, 99, String::new());
    store.take_actions();
    store.fail_views_request();
    store.load_views_slice(SELF_PEER, 99, "next_tok_3".to_owned());
    assert_eq!(store.take_actions().len(), 1);
}

#[test]
fn send_story_and_edit_story_privacy_media_areas_and_wire_shapes() {
    let self_u = input_user(SELF_USER, 11);
    let peer_a_u = input_user(PEER_USER_A, 22);
    let peer_b_u = input_user(PEER_USER_B, 33);

    // 1. Privacy rules match `StoryPrivacyToInput` (`api/api_stories.cpp:252-317`).
    let everyone = story_privacy_to_tl(
        &StoryPrivacyRuleInput {
            option: StoryPrivacyOption::Everyone,
            always_users: vec![],
            always_chats: vec![],
            never_users: vec![peer_a_u.clone()],
            never_chats: vec![555],
        },
        self_u.clone(),
    );
    assert_eq!(everyone.len(), 3);
    assert!(matches!(
        everyone[0],
        tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll
    ));
    assert!(matches!(
        everyone[1],
        tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(_)
    ));
    assert!(matches!(
        everyone[2],
        tl_enums::InputPrivacyRule::InputPrivacyValueDisallowChatParticipants(_)
    ));

    // CloseFriends emits ONLY AllowCloseFriends even if never/always were populated.
    let close_friends = story_privacy_to_tl(
        &StoryPrivacyRuleInput {
            option: StoryPrivacyOption::CloseFriends,
            always_users: vec![peer_a_u.clone()],
            always_chats: vec![123],
            never_users: vec![peer_b_u.clone()],
            never_chats: vec![456],
        },
        self_u.clone(),
    );
    assert_eq!(
        close_friends,
        vec![tl_enums::InputPrivacyRule::InputPrivacyValueAllowCloseFriends]
    );

    // Nobody with empty always lists falls back to allowing self_user.
    let nobody_empty = story_privacy_to_tl(
        &StoryPrivacyRuleInput {
            option: StoryPrivacyOption::Nobody,
            always_users: vec![],
            always_chats: vec![],
            never_users: vec![],
            never_chats: vec![],
        },
        self_u.clone(),
    );
    assert_eq!(
        nobody_empty,
        vec![tl_enums::InputPrivacyRule::InputPrivacyValueAllowUsers(
            tl_types::InputPrivacyValueAllowUsers {
                users: vec![self_u.clone()],
            }
        )]
    );

    // 2. MediaAreas match `MediaAreasToMTP` (`api/api_stories.cpp:145-231`).
    let area = StoryArea {
        geometry: RectF::new(0.25, 0.25, 0.5, 0.2),
        rotation: 15.0,
        radius: 12.0,
    };
    let areas = media_areas_to_tl(&StoryMediaAreasInput {
        reactions: vec![SuggestedReaction {
            area,
            reaction: ReactionId::Emoji("\u{1F44D}".to_owned()),
            count: 0,
            flipped: true,
            dark: true,
        }],
        urls: vec![UrlArea {
            area,
            url: "https://example.test/story".to_owned(),
        }],
        locations: vec![StoryLocation {
            area,
            point: GeoPoint {
                lat: 48.8566,
                lon: 2.3522,
                accuracy_radius: None,
            },
            country_iso2: "FR".to_owned(),
            city: "Paris".to_owned(),
            ..StoryLocation::default()
        }],
        weather: vec![WeatherArea {
            area,
            emoji: "\u{2600}".to_owned(),
            color: 0x00FF_AA00,
            millicelsius: 23_500,
        }],
        channel_posts: vec![(
            ChannelPost {
                area,
                item_id: FullMsgId::new(CHANNEL_PEER, MsgId(88)),
            },
            tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                channel_id: CHANNEL_ID.to_tl(),
                access_hash: 999,
            }),
        )],
    });
    assert_eq!(areas.len(), 5);
    assert!(
        matches!(areas[0], tl_enums::MediaArea::SuggestedReaction(ref r) if r.dark && r.flipped)
    );
    assert!(matches!(areas[1], tl_enums::MediaArea::Url(_)));
    assert!(matches!(areas[2], tl_enums::MediaArea::GeoPoint(ref g) if g.address.is_some()));
    assert!(
        matches!(areas[3], tl_enums::MediaArea::Weather(ref w) if (w.temperature_c - 23.5).abs() < 1e-6)
    );
    assert!(
        matches!(areas[4], tl_enums::MediaArea::InputMediaAreaChannelPost(ref cp) if cp.msg_id == 88)
    );

    // 3. `stories.sendStory` wire round-trip with repost, period, privacy, media_areas, albums.
    let send_req = build_send_story(SendStoryInput {
        peer: tl_enums::InputPeer::Self_,
        media: tl_enums::InputMedia::Empty,
        media_areas: areas.clone(),
        caption: "Sunset".to_owned(),
        entities: vec![tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
            offset: 0,
            length: 6,
        })],
        privacy_rules: everyone.clone(),
        random_id: 777_888_999,
        period: Some(86_400),
        pinned: true,
        noforwards: true,
        repost: Some(StoryRepostInput {
            from_peer: input_peer_user(PEER_USER_A, 22),
            from_story_id: 19,
            modified: true,
        }),
        albums: vec![7],
        music: Some(tl_enums::InputDocument::InputDocument(
            tl_types::InputDocument {
                id: DocumentId::from_tl(500).to_tl(),
                access_hash: 501,
                file_reference: vec![9, 9],
            },
        )),
    });
    let send_wire = send_req.to_bytes();
    let send_decoded = tl_fn::stories::SendStory::from_bytes(&send_wire).unwrap();
    assert_eq!(send_decoded, send_req);
    assert!(send_decoded.pinned);
    assert!(send_decoded.noforwards);
    assert!(send_decoded.fwd_modified);
    assert_eq!(send_decoded.period, Some(86_400));
    assert_eq!(send_decoded.fwd_from_story, Some(19));
    assert_eq!(send_decoded.albums, Some(vec![7]));

    // 4. `stories.editStory`: caption & entities share `flags.1`; `media` sets `media_areas`.
    let edit_req = build_edit_story(EditStoryInput {
        peer: tl_enums::InputPeer::Self_,
        id: 42,
        media: Some(tl_enums::InputMedia::Empty),
        media_areas: None,
        caption: Some(("Updated".to_owned(), vec![])),
        privacy_rules: Some(close_friends),
        music: None,
    });
    let edit_wire = edit_req.to_bytes();
    let edit_decoded = tl_fn::stories::EditStory::from_bytes(&edit_wire).unwrap();
    assert_eq!(edit_decoded, edit_req);
    assert_eq!(edit_decoded.caption.as_deref(), Some("Updated"));
    assert_eq!(edit_decoded.entities, Some(vec![]));
    assert_eq!(edit_decoded.media_areas, Some(vec![]));
}

#[test]
fn stealth_mode_reactions_pins_deletes_and_album_builders_wire_roundtrip() {
    let stealth = build_activate_stealth_mode(true, true);
    assert!(stealth.past && stealth.future);
    let _ = tl_fn::stories::ActivateStealthMode::from_bytes(&stealth.to_bytes()).unwrap();

    // Stealth mode evaluation helper matches `media_stories_stealth.cpp`.
    let dec_active = evaluate_stealth_mode(
        StealthMode {
            enabled_till: NOW + 600,
            cooldown_till: NOW + 3600,
        },
        NOW,
        true,
    );
    assert_eq!(
        dec_active,
        tg_stories::StealthActivationDecision::AlreadyActive { seconds_left: 600 }
    );
    let dec_cooldown = evaluate_stealth_mode(
        StealthMode {
            enabled_till: NOW - 10,
            cooldown_till: NOW + 3600,
        },
        NOW,
        true,
    );
    assert_eq!(
        dec_cooldown,
        tg_stories::StealthActivationDecision::InCooldown { seconds_left: 3600 }
    );

    // `stories.sendReaction` (default add_to_recent = false per data_stories.cpp:1124).
    let react = build_send_reaction(
        input_peer_user(PEER_USER_A, 1),
        12,
        Some(&ReactionId::Emoji("\u{1F525}".to_owned())),
        false,
    );
    assert!(!react.add_to_recent);
    assert_eq!(
        tl_fn::stories::SendReaction::from_bytes(&react.to_bytes()).unwrap(),
        react
    );

    // Remaining builders wire round-trips.
    let peer = input_peer_user(SELF_USER, 1);
    let _ = tl_fn::stories::GetPeerStories::from_bytes(
        &build_get_peer_stories(peer.clone()).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::GetStoriesByID::from_bytes(
        &build_get_stories_by_id(peer.clone(), vec![1, 2]).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::GetPinnedStories::from_bytes(
        &build_get_pinned_stories(peer.clone(), 0, 10).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::GetStoriesArchive::from_bytes(
        &build_get_stories_archive(peer.clone(), 0, 10).to_bytes(),
    )
    .unwrap();
    let _ =
        tl_fn::stories::ReadStories::from_bytes(&build_read_stories(peer.clone(), 55).to_bytes())
            .unwrap();
    let _ = tl_fn::stories::IncrementStoryViews::from_bytes(
        &build_increment_story_views(peer.clone(), vec![1, 2]).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::TogglePinned::from_bytes(
        &build_toggle_pinned(peer.clone(), vec![1], true).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::TogglePinnedToTop::from_bytes(
        &build_toggle_pinned_to_top(peer.clone(), vec![1, 2]).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::DeleteStories::from_bytes(
        &build_delete_stories(peer.clone(), vec![3]).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::GetAlbums::from_bytes(&build_get_albums(peer.clone(), 123).to_bytes())
        .unwrap();
    let _ = tl_fn::stories::GetAlbumStories::from_bytes(
        &build_get_album_stories(peer.clone(), 7, 0, 10).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::CreateAlbum::from_bytes(
        &build_create_album(peer.clone(), "Summer".to_owned(), vec![1, 2]).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::UpdateAlbum::from_bytes(
        &build_rename_album(peer.clone(), 7, "Winter".to_owned()).to_bytes(),
    )
    .unwrap();
    let _ = tl_fn::stories::UpdateAlbum::from_bytes(
        &build_reorder_album(peer.clone(), 7, vec![2, 1]).to_bytes(),
    )
    .unwrap();
    let upd_stories = build_update_album_stories(peer.clone(), 7, vec![3], vec![1]);
    assert_eq!(upd_stories.add_stories, Some(vec![3]));
    assert_eq!(upd_stories.delete_stories, Some(vec![1]));
    let _ = tl_fn::stories::UpdateAlbum::from_bytes(&upd_stories.to_bytes()).unwrap();
    let _ = tl_fn::stories::ReorderAlbums::from_bytes(
        &build_reorder_albums(peer.clone(), vec![9, 7]).to_bytes(),
    )
    .unwrap();
    let _ =
        tl_fn::stories::DeleteAlbum::from_bytes(&build_delete_album(peer.clone(), 7).to_bytes())
            .unwrap();
    let _ =
        tl_fn::stories::CanSendStory::from_bytes(&build_can_send_story(peer).to_bytes()).unwrap();
    let _ =
        tl_fn::stories::GetChatsToSend::from_bytes(&build_get_chats_to_send().to_bytes()).unwrap();

    // Story updates parsing: updateReadStories, updateStoriesStealthMode, updateStoryID.
    let u_read = tl_enums::Update::ReadStories(tl_types::UpdateReadStories {
        peer: tl_enums::Peer::User(tl_types::PeerUser {
            user_id: PEER_USER_A.to_tl(),
        }),
        max_id: 99,
    });
    assert_eq!(
        tg_stories::story_update_from_tl(&u_read, NOW, |_| StoryPeerContext::default()),
        Some(ParsedStoryUpdate::ReadStories {
            peer_id: PEER_A,
            max_id: 99,
        })
    );
    let u_id = tl_enums::Update::StoryID(tl_types::UpdateStoryID {
        id: 123,
        random_id: 9999,
    });
    assert_eq!(
        tg_stories::story_update_from_tl(&u_id, NOW, |_| StoryPeerContext::default()),
        Some(ParsedStoryUpdate::StoryId {
            id: 123,
            random_id: 9999,
        })
    );
    let u_del = tl_enums::Update::Story(tl_types::UpdateStory {
        peer: tl_enums::Peer::User(tl_types::PeerUser {
            user_id: PEER_USER_A.to_tl(),
        }),
        story: tl_enums::StoryItem::Deleted(tl_types::StoryItemDeleted { id: 12 }),
    });
    assert_eq!(
        tg_stories::story_update_from_tl(&u_del, NOW, |_| StoryPeerContext::default()),
        Some(ParsedStoryUpdate::Story {
            peer_id: PEER_A,
            item: ParsedStoryItem::Deleted(12),
        })
    );
}
