//! Live RPC tests for WP-26 profile extras, peer boxes, and Business-bot bar (`live/profile_extras.rs`):
//! - Profile Stories & Albums (`stories.getPinnedStories`, `getStoriesArchive`, `getAlbums`, `getAlbumStories`)
//! - Peer Star Gifts (`payments.getSavedStarGifts`, `getStarGiftCollections`, `saveStarGift`)
//! - Saved Music on Profile (`users.getSavedMusic`, `account.saveMusic`)
//! - Emoji Userpic Builder (`photos.uploadProfilePhoto`, `channels.editPhoto`, `messages.editChatPhoto`)
//! - Per-chat available reactions (`messages.setChatAvailableReactions`)
//! - Collectible Usernames (`channels.{toggle,reorder}Usernames`, `account.*`, `bots.*`)
//! - Bot custom verification (`bots.setCustomVerification`)
//! - Business-bot bar (`messages.getPeerSettings`, `account.toggleConnectedBotPaused`, `account.disablePeerConnectedBot`)
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs/users.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{ChannelFlags, ChannelId, ChatId, Peer, PeerId as DataPeerId, UserId as DataUserId};
use tg_payments::star_gifts::PeerGiftsFilter;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::profile_extras::refresh_business_bot_bar;
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::{
    ChatAvailableReactionsMode, EmojiUserpicRequest, InfoBackend, PeerReactionsConfig,
    default_standard_reactions,
};
use tg_app_core::stand_in::types::PeerId;

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_rpc_err(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value: None,
            })),
        ));
    }

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };

        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn synthetic_tl_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    is_bot: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !is_bot,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: true,
        attach_menu_enabled: false,
        bot_can_edit: is_bot,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: is_bot,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 13 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: is_bot.then_some(1),
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn synthetic_tl_channel(
    id: i64,
    title: &str,
    username: &str,
    broadcast: bool,
) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified: false,
        megagroup: !broadcast,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 29 + 11),
        title: title.to_owned(),
        username: (!username.is_empty()).then(|| username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_795_356_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(120),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

fn synthetic_tl_chat(id: i64, title: &str) -> tl_types::Chat {
    tl_types::Chat {
        creator: true,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 8,
        date: 1_795_356_000,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    }
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_356_000,
        seq: 0,
    })
}

const SELF_ID: i64 = 4001;
const CONTACT_ID: i64 = 4002;
const BOT_ID: i64 = 4003;
const CHANNEL_ID: i64 = 7001;
const BASIC_CHAT_ID: i64 = 5001;

struct ProfileExtrasHarness {
    _runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>,
    rpc: FakeRpcClient,
    live: LiveSession,
}

impl ProfileExtrasHarness {
    fn start() -> Self {
        let rpc = FakeRpcClient::new(2);
        let cfg = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        };
        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(SELF_ID),
                crate::ErasedRpcClient::new(rpc.clone()),
                cfg,
            )
            .expect("start SessionRuntime"),
        );
        runtime.tokio_handle().block_on(async {
            let mut sess = runtime.session().write().await;
            let users = vec![
                tl_enums::User::User(synthetic_tl_user(
                    SELF_ID,
                    "Taylor",
                    "taylor_self",
                    true,
                    false,
                )),
                tl_enums::User::User(synthetic_tl_user(
                    CONTACT_ID, "Mira", "mira_v", false, false,
                )),
                tl_enums::User::User(synthetic_tl_user(
                    BOT_ID,
                    "Verify Bot",
                    "verify_helper_bot",
                    false,
                    true,
                )),
            ];
            let chats = vec![
                tl_enums::Chat::Channel(synthetic_tl_channel(
                    CHANNEL_ID,
                    "Release Feed",
                    "release_feed",
                    true,
                )),
                tl_enums::Chat::Chat(synthetic_tl_chat(BASIC_CHAT_ID, "Small Team Chat")),
            ];
            apply_tl_users_and_chats(&mut sess, &users, &chats);
            if let Some(Peer::Channel(ch)) =
                sess.peer_mut(DataPeerId::from_channel(ChannelId(CHANNEL_ID as u64)))
            {
                ch.flags |= ChannelFlags::CREATOR | ChannelFlags::BROADCAST;
            }
        });

        let live = LiveSession::new(Arc::clone(&runtime), 26);
        Self {
            _runtime: runtime,
            rpc,
            live,
        }
    }

    fn self_peer() -> PeerId {
        PeerId::from_user(SELF_ID as u64)
    }

    fn contact_peer() -> PeerId {
        PeerId::from_user(CONTACT_ID as u64)
    }

    fn bot_peer() -> PeerId {
        PeerId::from_user(BOT_ID as u64)
    }

    fn channel_peer() -> PeerId {
        PeerId::from_channel(CHANNEL_ID as u64)
    }

    fn chat_peer() -> PeerId {
        PeerId::from_chat(ChatId(BASIC_CHAT_ID as u64).0)
    }
}

fn sample_tl_story_item(
    id: i32,
    caption: &str,
    pinned: bool,
    albums: Option<Vec<i32>>,
) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out: true,
        id,
        date: 1_795_350_000,
        from_id: None,
        fwd_from: None,
        expire_date: 1_795_436_400,
        caption: Some(caption.to_owned()),
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Empty),
        media_areas: None,
        privacy: None,
        views: Some(tl_enums::StoryViews::StoryViews(tl_types::StoryViews {
            has_viewers: false,
            views_count: 321,
            forwards_count: None,
            reactions: None,
            reactions_count: None,
            recent_viewers: None,
        })),
        sent_reaction: None,
        albums,
        music: None,
    })
}

#[test]
fn live_profile_stories_archive_and_albums_rpcs() {
    let h = ProfileExtrasHarness::start();
    let peer = ProfileExtrasHarness::contact_peer();

    h.rpc.enqueue_ok(
        tl_fns::stories::GetPinnedStories::CONSTRUCTOR_ID,
        &tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
            count: 2,
            stories: vec![
                sample_tl_story_item(101, "Pinned sunrise", true, Some(vec![7])),
                sample_tl_story_item(102, "Pinned demo", true, None),
            ],
            pinned_to_top: Some(vec![101]),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let pinned = h
        .live
        .peer_pinned_stories(peer, 0, 20)
        .expect("pinned stories");
    assert_eq!(pinned.total_count, 2);
    assert_eq!(pinned.stories.len(), 2);
    assert_eq!(pinned.stories[0].caption, "Pinned sunrise");
    assert_eq!(pinned.stories[0].views_count, 321);
    assert_eq!(pinned.pinned_to_top, vec![101]);

    h.rpc.enqueue_ok(
        tl_fns::stories::GetStoriesArchive::CONSTRUCTOR_ID,
        &tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
            count: 1,
            stories: vec![sample_tl_story_item(55, "Archived note", false, None)],
            pinned_to_top: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let archive = h
        .live
        .peer_stories_archive(peer, 0, 20)
        .expect("archive stories");
    assert_eq!(archive.stories.len(), 1);
    assert_eq!(archive.stories[0].id, 55);

    h.rpc.enqueue_ok(
        tl_fns::stories::GetAlbums::CONSTRUCTOR_ID,
        &tl_enums::stories::Albums::Albums(tl_types::stories::Albums {
            hash: 42,
            albums: vec![tl_enums::StoryAlbum::StoryAlbum(tl_types::StoryAlbum {
                album_id: 7,
                title: "Outdoors".to_owned(),
                icon_photo: None,
                icon_video: None,
            })],
        }),
    );
    let albums = h.live.peer_story_albums(peer).expect("albums");
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].album_id, 7);
    assert_eq!(albums[0].title, "Outdoors");

    h.rpc.enqueue_ok(
        tl_fns::stories::GetAlbumStories::CONSTRUCTOR_ID,
        &tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
            count: 1,
            stories: vec![sample_tl_story_item(
                101,
                "Pinned sunrise",
                true,
                Some(vec![7]),
            )],
            pinned_to_top: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let album_page = h
        .live
        .peer_album_stories(peer, 7, 0, 20)
        .expect("album stories");
    assert_eq!(album_page.stories.len(), 1);
    assert_eq!(album_page.stories[0].album_ids, vec![7]);
}

#[test]
fn live_peer_saved_gifts_and_save_toggle_rpcs() {
    let mut h = ProfileExtrasHarness::start();
    let peer = ProfileExtrasHarness::contact_peer();

    let gift = tl_enums::StarGift::StarGift(tl_types::StarGift {
        limited: true,
        sold_out: false,
        birthday: false,
        require_premium: false,
        limited_per_user: false,
        peer_color_available: false,
        auction: false,
        id: 8801,
        sticker: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 901 }),
        stars: 150,
        availability_remains: Some(50),
        availability_total: Some(500),
        availability_resale: None,
        convert_stars: 125,
        first_sale_date: None,
        last_sale_date: None,
        upgrade_stars: Some(25),
        resell_min_stars: None,
        title: Some("Star Lantern".to_owned()),
        released_by: None,
        per_user_total: None,
        per_user_remains: None,
        locked_until_date: None,
        auction_slug: None,
        gifts_per_round: None,
        auction_start_date: None,
        upgrade_variants: None,
        background: None,
    });
    let saved = tl_enums::SavedStarGift::SavedStarGift(tl_types::SavedStarGift {
        name_hidden: false,
        unsaved: false,
        refunded: false,
        can_upgrade: true,
        pinned_to_top: true,
        upgrade_separate: false,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: SELF_ID,
        })),
        date: 1_795_350_000,
        gift,
        message: Some(tl_enums::TextWithEntities::TextWithEntities(
            tl_types::TextWithEntities {
                text: "Cheers!".to_owned(),
                entities: Vec::new(),
            },
        )),
        msg_id: Some(9011),
        saved_id: None,
        convert_stars: Some(125),
        upgrade_stars: Some(25),
        can_export_at: None,
        transfer_stars: None,
        can_transfer_at: None,
        can_resell_at: None,
        collection_id: Some(vec![3]),
        prepaid_upgrade_hash: None,
        drop_original_details_stars: None,
        gift_num: None,
        can_craft_at: None,
    });

    h.rpc.enqueue_ok(
        tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 1,
            chat_notifications_enabled: None,
            gifts: vec![saved],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.rpc.enqueue_ok(
        tl_fns::payments::GetStarGiftCollections::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftCollections::StarGiftCollections(
            tl_types::payments::StarGiftCollections {
                collections: vec![tl_enums::StarGiftCollection::StarGiftCollection(
                    tl_types::StarGiftCollection {
                        collection_id: 3,
                        title: "Favorites".to_owned(),
                        icon: None,
                        gifts_count: 1,
                        hash: 99,
                    },
                )],
            },
        ),
    );

    let page = h
        .live
        .peer_saved_gifts(peer, &PeerGiftsFilter::default())
        .expect("saved gifts");
    assert_eq!(page.total_count, 1);
    assert_eq!(page.gifts.len(), 1);
    assert_eq!(page.gifts[0].title, "Star Lantern");
    assert_eq!(page.collections.len(), 1);
    assert_eq!(page.collections[0].title, "Favorites");

    h.rpc
        .enqueue_ok(tl_fns::payments::SaveStarGift::CONSTRUCTOR_ID, &true);
    h.live
        .toggle_peer_gift_saved(peer, 9011, 0, true)
        .expect("hide gift");

    let calls = h.rpc.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::payments::SaveStarGift::CONSTRUCTOR_ID)
    );
}

#[test]
fn live_saved_music_get_and_save_rpcs() {
    let mut h = ProfileExtrasHarness::start();
    let peer = ProfileExtrasHarness::contact_peer();

    let doc = tl_enums::Document::Document(tl_types::Document {
        id: 55_001,
        access_hash: 77_002,
        file_reference: vec![0xAA, 0xBB],
        date: 1_795_350_000,
        mime_type: "audio/mpeg".to_owned(),
        size: 3_456_789,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl_enums::DocumentAttribute::Audio(
            tl_types::DocumentAttributeAudio {
                voice: false,
                duration: 195,
                title: Some("Orbit Theme".to_owned()),
                performer: Some("Synth Trio".to_owned()),
                waveform: None,
            },
        )],
    });
    h.rpc.enqueue_ok(
        tl_fns::users::GetSavedMusic::CONSTRUCTOR_ID,
        &tl_enums::users::SavedMusic::SavedMusic(tl_types::users::SavedMusic {
            count: 1,
            documents: vec![doc],
        }),
    );

    let tracks = h.live.peer_saved_music(peer, 0, 20).expect("saved music");
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Orbit Theme");
    assert_eq!(tracks[0].performer, "Synth Trio");
    assert_eq!(tracks[0].duration_secs, 195);

    h.rpc
        .enqueue_ok(tl_fns::account::SaveMusic::CONSTRUCTOR_ID, &true);
    h.live
        .save_profile_music(&tracks[0], true)
        .expect("unsave music");

    let save_call = h
        .rpc
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::account::SaveMusic::CONSTRUCTOR_ID)
        .expect("account.saveMusic recorded");
    let decoded = tl_fns::account::SaveMusic::from_bytes(&save_call.1).expect("decode SaveMusic");
    assert!(decoded.unsave);
}

#[test]
fn live_emoji_userpic_upload_user_channel_and_chat_rpcs() {
    let mut h = ProfileExtrasHarness::start();
    let req = EmojiUserpicRequest {
        emoji_id: 5368324170671202286,
        emoji_glyph: "🚀".to_owned(),
        background_colors: vec![0x54A9EB, 0x2B7DE9],
    };

    // 1. Self profile -> photos.uploadProfilePhoto
    h.rpc.enqueue_ok(
        tl_fns::photos::UploadProfilePhoto::CONSTRUCTOR_ID,
        &tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 1 }),
            users: Vec::new(),
        }),
    );
    h.live
        .upload_emoji_userpic(ProfileExtrasHarness::self_peer(), req.clone())
        .expect("self emoji userpic");

    // 2. Channel -> channels.editPhoto
    h.rpc.enqueue_ok(
        tl_fns::channels::EditPhoto::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .upload_emoji_userpic(ProfileExtrasHarness::channel_peer(), req.clone())
        .expect("channel emoji userpic");

    // 3. Basic chat -> messages.editChatPhoto
    h.rpc.enqueue_ok(
        tl_fns::messages::EditChatPhoto::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .upload_emoji_userpic(ProfileExtrasHarness::chat_peer(), req)
        .expect("chat emoji userpic");

    let calls = h.rpc.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::photos::UploadProfilePhoto::CONSTRUCTOR_ID)
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::channels::EditPhoto::CONSTRUCTOR_ID)
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::EditChatPhoto::CONSTRUCTOR_ID)
    );

    // Invalid empty colors rejected locally before RPC
    let bad = EmojiUserpicRequest {
        emoji_id: 1,
        emoji_glyph: "⭐".to_owned(),
        background_colors: Vec::new(),
    };
    assert!(
        h.live
            .upload_emoji_userpic(ProfileExtrasHarness::self_peer(), bad)
            .is_err()
    );
}

#[test]
fn live_set_chat_available_reactions_all_some_none() {
    let mut h = ProfileExtrasHarness::start();
    let ch = ProfileExtrasHarness::channel_peer();

    h.rpc.enqueue_ok(
        tl_fns::messages::SetChatAvailableReactions::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let cfg = PeerReactionsConfig {
        mode: ChatAvailableReactionsMode::Some {
            emojis: vec!["👍".to_owned(), "🔥".to_owned()],
            custom_emoji_ids: vec![998877],
        },
        reactions_limit: Some(5),
        paid_enabled: true,
        available_standard: default_standard_reactions(),
    };
    h.live
        .set_peer_available_reactions(ch, cfg.clone())
        .expect("set Some reactions");
    assert_eq!(h.live.peer_available_reactions(ch), cfg);

    let call = h
        .rpc
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::messages::SetChatAvailableReactions::CONSTRUCTOR_ID)
        .expect("setChatAvailableReactions recorded");
    let decoded = tl_fns::messages::SetChatAvailableReactions::from_bytes(&call.1)
        .expect("decode setChatAvailableReactions");
    assert_eq!(decoded.reactions_limit, Some(5));
    assert_eq!(decoded.paid_enabled, Some(true));
    assert!(matches!(
        decoded.available_reactions,
        tl_enums::ChatReactions::Some(ref s) if s.reactions.len() == 3
    ));
}

#[test]
fn live_collectible_usernames_toggle_and_reorder_rpcs() {
    let mut h = ProfileExtrasHarness::start();

    // Channel
    h.rpc
        .enqueue_ok(tl_fns::channels::ToggleUsername::CONSTRUCTOR_ID, &true);
    h.rpc
        .enqueue_ok(tl_fns::channels::ReorderUsernames::CONSTRUCTOR_ID, &true);
    h.live
        .toggle_peer_username(ProfileExtrasHarness::channel_peer(), "@release_nft", false)
        .expect("channel toggleUsername");
    h.live
        .reorder_peer_usernames(
            ProfileExtrasHarness::channel_peer(),
            &["release_feed".to_owned(), "release_alt".to_owned()],
        )
        .expect("channel reorderUsernames");

    // Self account
    h.rpc
        .enqueue_ok(tl_fns::account::ToggleUsername::CONSTRUCTOR_ID, &true);
    h.rpc
        .enqueue_ok(tl_fns::account::ReorderUsernames::CONSTRUCTOR_ID, &true);
    h.live
        .toggle_peer_username(ProfileExtrasHarness::self_peer(), "taylor_nft", true)
        .expect("account toggleUsername");
    h.live
        .reorder_peer_usernames(
            ProfileExtrasHarness::self_peer(),
            &["taylor_nft".to_owned(), "taylor_self".to_owned()],
        )
        .expect("account reorderUsernames");

    // Owned bot
    h.rpc
        .enqueue_ok(tl_fns::bots::ToggleUsername::CONSTRUCTOR_ID, &true);
    h.rpc
        .enqueue_ok(tl_fns::bots::ReorderUsernames::CONSTRUCTOR_ID, &true);
    h.live
        .toggle_peer_username(ProfileExtrasHarness::bot_peer(), "bot_nft", true)
        .expect("bots toggleUsername");
    h.live
        .reorder_peer_usernames(
            ProfileExtrasHarness::bot_peer(),
            &["bot_nft".to_owned(), "verify_helper_bot".to_owned()],
        )
        .expect("bots reorderUsernames");

    let ctors: Vec<u32> = h.rpc.recorded_calls().into_iter().map(|(c, _)| c).collect();
    assert!(ctors.contains(&tl_fns::channels::ToggleUsername::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::channels::ReorderUsernames::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::account::ToggleUsername::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::account::ReorderUsernames::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::bots::ToggleUsername::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::bots::ReorderUsernames::CONSTRUCTOR_ID));
}

#[test]
fn live_bot_custom_verification_and_business_bot_bar_rpcs() {
    let mut h = ProfileExtrasHarness::start();
    let bot = ProfileExtrasHarness::bot_peer();
    let contact = ProfileExtrasHarness::contact_peer();

    // 1. bots.setCustomVerification enable + disable + length validation
    h.rpc
        .enqueue_ok(tl_fns::bots::SetCustomVerification::CONSTRUCTOR_ID, &true);
    h.live
        .set_peer_custom_verification(bot, contact, true, Some("Verified contributor"))
        .expect("enable verification");
    let st = h.live.peer_verification_status(bot, contact);
    assert!(st.verified);
    assert_eq!(st.custom_description, "Verified contributor");

    let too_long = "x".repeat(71);
    assert!(
        h.live
            .set_peer_custom_verification(bot, contact, true, Some(&too_long))
            .is_err()
    );

    h.rpc
        .enqueue_ok(tl_fns::bots::SetCustomVerification::CONSTRUCTOR_ID, &true);
    h.live
        .set_peer_custom_verification(bot, contact, false, None)
        .expect("revoke verification");
    assert!(!h.live.peer_verification_status(bot, contact).verified);

    // 2. Business-bot bar: refresh via messages.getPeerSettings -> pause -> resume -> disable
    h.rpc.enqueue_ok(
        tl_fns::messages::GetPeerSettings::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerSettings::PeerSettings(tl_types::messages::PeerSettings {
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
                business_bot_can_reply: true,
                geo_distance: None,
                request_chat_title: None,
                request_chat_date: None,
                business_bot_id: Some(BOT_ID),
                business_bot_manage_url: Some(
                    "https://t.me/verify_helper_bot?start=biz".to_owned(),
                ),
                charge_paid_message_stars: None,
                registration_month: None,
                phone_country: None,
                name_change_date: None,
                photo_change_date: None,
            }),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let bar = refresh_business_bot_bar(&h.live, contact)
        .expect("getPeerSettings")
        .expect("business bot present");
    assert_eq!(bar.bot_id, bot);
    assert!(bar.can_reply);
    assert!(!bar.paused);

    h.rpc.enqueue_ok(
        tl_fns::account::ToggleConnectedBotPaused::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .toggle_business_bot_paused(contact, true)
        .expect("pause business bot");
    assert!(
        h.live
            .business_bot_bar(contact)
            .expect("bar still cached")
            .paused
    );

    h.rpc.enqueue_ok(
        tl_fns::account::ToggleConnectedBotPaused::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .toggle_business_bot_paused(contact, false)
        .expect("resume business bot");
    assert!(
        !h.live
            .business_bot_bar(contact)
            .expect("bar still cached")
            .paused
    );

    h.rpc.enqueue_ok(
        tl_fns::account::DisablePeerConnectedBot::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .disable_business_bot(contact)
        .expect("disable business bot");
    assert!(h.live.business_bot_bar(contact).is_none());

    // Error propagation check
    h.rpc.enqueue_rpc_err(
        tl_fns::account::ToggleConnectedBotPaused::CONSTRUCTOR_ID,
        400,
        "BOT_BUSINESS_MISSING",
    );
    let err = h
        .live
        .toggle_business_bot_paused(contact, true)
        .expect_err("rpc error propagates");
    assert!(err.contains("BOT_BUSINESS_MISSING"));
}
