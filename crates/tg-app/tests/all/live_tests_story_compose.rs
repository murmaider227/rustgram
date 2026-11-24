//! Integration, RPC, and headless `egui_kittest` UI tests for the Story Composer (WP-22):
//! - `stories.sendStory` and `stories.editStory` request field verification against `FakeRpcClient`
//! - `StoriesBackend::send_story` / `edit_story` on `LiveSession`, `MockStoriesBackend`, and `MockSession`
//! - Deep-link exact `(peer_id, story_id)` opening in `AppStoriesHost::open_story_deep_link`
//! - `egui_kittest` tests for `StoriesStrip` ("+ Add Story" & upload progress), `SendStoryBox`,
//!   `StoryPrivacyBox` (including exceptions), and `StoryDraftsBox`.
//!
//! All tests run exclusively against synthetic users/media and an in-process `FakeRpcClient`.
//! No real Telegram network connections or credentials are ever used.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::peer::GeoPoint;
use tg_data::{FullStoryId, PeerId as DataPeerId, ReactionId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_stories::{
    RectF, StoriesStore, StoryArea, StoryLocation, StoryPrivacy, StoryUploadStatus,
    SuggestedReaction, UrlArea, WeatherArea,
};
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::live::{LiveSession, MultiAccountWorkspaceHost};
use tg_app::notifications::manager::NotificationSystem;
use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::theme::Palette;
use tg_app::startup::ipc::IncomingActivation;
use tg_app::stories::AppStoriesHost;
use tg_app::stories::backend::{
    EditStoryRequest, SendStoryRequest, StoriesBackend, StoryMediaAreasInput, StoryPrivacyInput,
    StoryUploadMedia,
};
use tg_app::stories::compose::{
    STORY_CAPTION_LIMIT_DEFAULT, SendStoryBox, parse_story_caption_entities,
};
use tg_app::stories::mock::{
    GradientPreset, MockStoriesBackend, generate_gradient_story_png, mock_peers,
};
use tg_app::tray::MockTrayPlatform;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const NOW_SECS: i32 = 1_760_000_000;

type RecordedCall = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    dynamic: Arc<Mutex<Option<DynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }

    fn recorded_calls_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(2, req).await
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
            .push((ctor, bytes.clone()));

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(ctor, &bytes)
        {
            let raw = res?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos).map(|(_, r)| r))
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn make_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
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
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 100 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
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

fn make_photo_story_item(id: i32, caption: &str, edited: bool) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: true,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited,
        contacts: false,
        selected_contacts: false,
        out: true,
        id,
        date: NOW_SECS,
        from_id: None,
        fwd_from: None,
        expire_date: NOW_SECS + 86_400,
        caption: Some(caption.to_owned()),
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler: false,
            live_photo: false,
            photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                has_stickers: false,
                id: id as i64 * 10,
                access_hash: id as i64 * 19,
                file_reference: vec![1, 2, 3],
                date: NOW_SECS,
                sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                    r#type: "y".to_owned(),
                    w: 360,
                    h: 640,
                    size: 2048,
                })],
                video_sizes: None,
                dc_id: 2,
            })),
            ttl_seconds: None,
            video: None,
        })),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    })
}

struct StoryComposeHarness {
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    live: LiveSession,
    self_peer: DataPeerId,
    contact_peer: DataPeerId,
}

impl StoryComposeHarness {
    fn start() -> Self {
        let fake = FakeRpcClient::new();
        let self_uid = 10_001_i64;
        let contact_uid = 20_002_i64;

        let config = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        };
        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                tg_app::live::ErasedRpcClient::new(fake.clone()),
                config,
            )
            .expect("start SessionRuntime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("ready");
        runtime.tokio_handle().block_on(async {
            let mut s = runtime.session().write().await;
            tg_api::convert::apply_tl_users_and_chats(
                &mut s,
                &[
                    tl_enums::User::User(make_tl_user(self_uid, "SelfAccount", "self_user", true)),
                    tl_enums::User::User(make_tl_user(
                        contact_uid,
                        "CedarContact",
                        "cedar_contact",
                        false,
                    )),
                ],
                &[],
            );
            let contact_peer = DataPeerId::from_user(DataUserId::from_tl(contact_uid));
            s.ensure_history(contact_peer).in_chat_list = true;
            s.sync_dialog_lists(contact_peer);
        });
        let live = LiveSession::new(Arc::clone(&runtime), 1);
        Self {
            fake,
            runtime,
            live,
            self_peer: DataPeerId::from_user(DataUserId::from_tl(self_uid)),
            contact_peer: DataPeerId::from_user(DataUserId::from_tl(contact_uid)),
        }
    }

    fn shutdown(self) {
        drop(self.live);
        if let Ok(rt) = Arc::try_unwrap(self.runtime) {
            rt.shutdown();
        }
    }
}

fn synth_photo_bytes() -> Vec<u8> {
    generate_gradient_story_png(
        64,
        112,
        GradientPreset {
            top: [0x14, 0x28, 0x50],
            mid: [0x27, 0x49, 0x6D],
            bottom: [0x0C, 0x7B, 0x93],
            accent: [0xFF, 0xD1, 0x66],
            orb_x: 0.6,
            orb_y: 0.3,
            stripes: false,
        },
    )
}

#[test]
fn parse_story_caption_entities_extracts_markdown_and_merges_explicit_utf16_spans() {
    let parsed = parse_story_caption_entities(
        "**Hi** _world_ `code` ||secret||",
        &[EntityInText::new(EntityType::Underline, 0, 2)],
    );
    assert_eq!(parsed.text, "Hi world code secret");
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Bold, 0, 2))
    );
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Underline, 0, 2))
    );
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Italic, 3, 5))
    );
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Code, 9, 4))
    );
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Spoiler, 14, 6))
    );
}

#[test]
fn stories_backend_trait_send_story_and_edit_story_encode_exact_tl_fields() {
    let mut h = StoryComposeHarness::start();
    let self_uid = h.self_peer.to_user().expect("self user").0 as i64;
    let contact_uid = h.contact_peer.to_user().expect("contact user").0 as i64;

    h.fake.set_dynamic_handler(Arc::new(move |ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::stories::SendStory::CONSTRUCTOR_ID {
            let updates = tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::Story(tl_types::UpdateStory {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: self_uid }),
                    story: make_photo_story_item(701, "Hello Story", false),
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 1,
            });
            return Some(Ok(updates.to_bytes()));
        }
        if ctor == tl_fns::stories::EditStory::CONSTRUCTOR_ID {
            let updates = tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::Story(tl_types::UpdateStory {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: self_uid }),
                    story: make_photo_story_item(701, "Edited Story", true),
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS + 10,
                seq: 2,
            });
            return Some(Ok(updates.to_bytes()));
        }
        if ctor == tl_fns::contacts::EditCloseFriends::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    let mut store = StoriesStore::new(h.self_peer);
    let backend: &mut dyn StoriesBackend = &mut h.live;

    // 1. StoriesBackend::send_story sends full photo + entities + privacy + areas.
    let area = StoryArea {
        geometry: RectF::new(0.2, 0.3, 0.5, 0.12),
        rotation: 5.0,
        radius: 10.0,
    };
    let created_id = backend
        .send_story(
            Some(&mut store),
            SendStoryRequest {
                peer_id: h.self_peer,
                media: StoryUploadMedia::Photo {
                    bytes: synth_photo_bytes(),
                    file_name: "desk_view.png".to_owned(),
                },
                caption: TextWithEntities {
                    text: "Hello Story".to_owned(),
                    entities: vec![EntityInText::new(EntityType::Bold, 0, 5)],
                },
                privacy: StoryPrivacyInput {
                    privacy: StoryPrivacy::Public,
                    allow_peers: Vec::new(),
                    disallow_peers: vec![h.contact_peer],
                },
                period: Some(21_600),
                pinned: true,
                noforwards: true,
                albums: vec![4],
                locations: vec![StoryLocation {
                    area,
                    point: GeoPoint {
                        lat: 47.6062,
                        lon: -122.3321,
                        accuracy_radius: Some(25),
                    },
                    title: "Harbor Point".to_owned(),
                    city: "Port Harbor".to_owned(),
                    country_iso2: "US".to_owned(),
                    ..StoryLocation::default()
                }],
                suggested_reactions: vec![SuggestedReaction {
                    area,
                    reaction: ReactionId::Emoji("\u{1F525}".to_owned()),
                    count: 0,
                    flipped: false,
                    dark: true,
                }],
                channel_posts: Vec::new(),
                url_areas: vec![UrlArea {
                    area,
                    url: "https://example.org/launch".to_owned(),
                }],
                weather_areas: vec![WeatherArea {
                    area,
                    emoji: "\u{2600}".to_owned(),
                    color: 0xEB_FF_A7_26,
                    millicelsius: 23_000,
                }],
            },
            NOW_SECS,
        )
        .expect("send_story via trait");
    assert_eq!(created_id, 701);
    assert!(store.lookup(FullStoryId::new(h.self_peer, 701)).is_ok());

    let send_calls = h
        .fake
        .recorded_calls_of(tl_fns::stories::SendStory::CONSTRUCTOR_ID);
    assert_eq!(send_calls.len(), 1);
    let send_req = tl_fns::stories::SendStory::from_bytes(&send_calls[0]).expect("decode send");
    assert!(send_req.pinned);
    assert!(send_req.noforwards);
    assert_eq!(send_req.period, Some(21_600));
    assert_eq!(send_req.caption.as_deref(), Some("Hello Story"));
    assert_eq!(send_req.entities.as_ref().map(Vec::len), Some(1));
    assert_eq!(send_req.albums, Some(vec![4]));
    assert_eq!(send_req.media_areas.as_ref().map(Vec::len), Some(4));

    // 2. Metadata-only edit_story: edits caption/entities, privacy (Contacts minus contact), and areas.
    let save_parts_before = h
        .fake
        .recorded_calls_of(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID)
        .len();
    backend
        .edit_story(
            Some(&mut store),
            EditStoryRequest {
                peer_id: h.self_peer,
                story_id: 701,
                media: None,
                media_areas: Some(StoryMediaAreasInput {
                    url_areas: vec![UrlArea {
                        area,
                        url: "https://example.org/edited".to_owned(),
                    }],
                    ..StoryMediaAreasInput::default()
                }),
                caption: Some(TextWithEntities {
                    text: "Edited Story".to_owned(),
                    entities: vec![EntityInText::new(EntityType::Italic, 0, 6)],
                }),
                privacy: Some(StoryPrivacyInput {
                    privacy: StoryPrivacy::Contacts,
                    allow_peers: Vec::new(),
                    disallow_peers: vec![h.contact_peer],
                }),
            },
            NOW_SECS + 10,
        )
        .expect("edit_story metadata-only");

    // No extra upload parts for metadata-only edit.
    assert_eq!(
        h.fake
            .recorded_calls_of(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID)
            .len(),
        save_parts_before
    );

    // 3. Privacy-only edit_story: caption and entities must BOTH be None (shared flags.1).
    backend
        .edit_story(
            Some(&mut store),
            EditStoryRequest {
                peer_id: h.self_peer,
                story_id: 701,
                media: None,
                media_areas: None,
                caption: None,
                privacy: Some(StoryPrivacyInput {
                    privacy: StoryPrivacy::SelectedContacts,
                    allow_peers: vec![h.contact_peer],
                    disallow_peers: Vec::new(),
                }),
            },
            NOW_SECS + 20,
        )
        .expect("edit_story privacy-only");

    // 4. Replacement-video edit_story: uploads video parts and sets InputMediaUploadedDocument.
    backend
        .edit_story(
            Some(&mut store),
            EditStoryRequest {
                peer_id: h.self_peer,
                story_id: 701,
                media: Some(StoryUploadMedia::Video {
                    bytes: vec![0x55; 8 * 1024],
                    file_name: "replace.mp4".to_owned(),
                    width: 720,
                    height: 1280,
                    duration_secs: 4.5,
                    has_audio: false,
                    thumb_bytes: None,
                }),
                media_areas: None,
                caption: None,
                privacy: None,
            },
            NOW_SECS + 30,
        )
        .expect("edit_story video replace");

    let edit_calls = h
        .fake
        .recorded_calls_of(tl_fns::stories::EditStory::CONSTRUCTOR_ID);
    assert_eq!(edit_calls.len(), 3);

    let edit0 = tl_fns::stories::EditStory::from_bytes(&edit_calls[0]).expect("decode edit 0");
    assert_eq!(edit0.id, 701);
    assert!(edit0.media.is_none());
    assert_eq!(edit0.caption.as_deref(), Some("Edited Story"));
    assert_eq!(edit0.entities.as_ref().map(Vec::len), Some(1));
    assert_eq!(edit0.media_areas.as_ref().map(Vec::len), Some(1));
    let rules0 = edit0.privacy_rules.expect("privacy rules 0");
    assert!(rules0.iter().any(|r| matches!(
        r,
        tl_enums::InputPrivacyRule::InputPrivacyValueAllowContacts
    )));
    assert!(rules0.iter().any(|r| matches!(
        r,
        tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(u) if u.users.len() == 1
    )));

    let edit1 = tl_fns::stories::EditStory::from_bytes(&edit_calls[1]).expect("decode edit 1");
    assert_eq!(edit1.id, 701);
    assert!(edit1.caption.is_none());
    assert!(edit1.entities.is_none());
    assert!(edit1.media.is_none());
    let rules1 = edit1.privacy_rules.expect("privacy rules 1");
    assert!(rules1.iter().any(|r| matches!(
        r,
        tl_enums::InputPrivacyRule::InputPrivacyValueAllowUsers(u) if u.users.len() == 1
    )));

    let edit2 = tl_fns::stories::EditStory::from_bytes(&edit_calls[2]).expect("decode edit 2");
    assert_eq!(edit2.id, 701);
    assert!(matches!(
        edit2.media,
        Some(tl_enums::InputMedia::UploadedDocument(ref d)) if d.nosound_video && d.mime_type == "video/mp4"
    ));

    // 5. Close Friends update invokes contacts.editCloseFriends and updates local session flags.
    backend
        .set_close_friends(vec![h.contact_peer])
        .expect("set_close_friends");
    assert_eq!(backend.close_friends(), vec![h.contact_peer]);
    let cf_calls = h
        .fake
        .recorded_calls_of(tl_fns::contacts::EditCloseFriends::CONSTRUCTOR_ID);
    assert_eq!(cf_calls.len(), 1);
    let cf_req = tl_fns::contacts::EditCloseFriends::from_bytes(&cf_calls[0]).expect("decode cf");
    assert_eq!(cf_req.id, vec![contact_uid]);

    // Verify contacts_for_privacy returns the known contact user.
    assert!(
        backend
            .contacts_for_privacy()
            .iter()
            .any(|p| p.peer_id == h.contact_peer)
    );

    h.shutdown();
}

#[test]
fn mock_session_supports_send_story_and_edit_story() {
    let mut session = MockSession::new(true);
    let self_id = StoriesBackend::self_peer_id(&session);
    let mut store = StoriesStore::new(self_id);
    session.execute_actions(
        &mut store,
        vec![tg_stories::StoriesAction::GetAllStories {
            list: tg_stories::StorySourcesList::NotHidden,
            state: None,
        }],
        NOW_SECS,
    );

    let new_id = StoriesBackend::send_story(
        &mut session,
        Some(&mut store),
        SendStoryRequest {
            peer_id: self_id,
            media: StoryUploadMedia::Photo {
                bytes: synth_photo_bytes(),
                file_name: "mock_story.png".to_owned(),
            },
            caption: TextWithEntities {
                text: "Mock caption".to_owned(),
                entities: vec![EntityInText::new(EntityType::Bold, 0, 4)],
            },
            privacy: StoryPrivacyInput {
                privacy: StoryPrivacy::CloseFriends,
                allow_peers: Vec::new(),
                disallow_peers: Vec::new(),
            },
            period: Some(43_200),
            pinned: true,
            noforwards: true,
            albums: Vec::new(),
            locations: Vec::new(),
            suggested_reactions: Vec::new(),
            channel_posts: Vec::new(),
            url_areas: Vec::new(),
            weather_areas: Vec::new(),
        },
        NOW_SECS,
    )
    .expect("MockSession::send_story");

    let full_id = FullStoryId::new(self_id, new_id);
    let story = store.lookup(full_id).expect("posted story in store");
    assert_eq!(story.caption.text, "Mock caption");
    assert_eq!(story.privacy, StoryPrivacy::CloseFriends);
    assert!(story.no_forwards);
    assert!(StoriesBackend::load_story_media(&mut session, full_id).is_some());

    StoriesBackend::edit_story(
        &mut session,
        Some(&mut store),
        EditStoryRequest {
            peer_id: self_id,
            story_id: new_id,
            media: None,
            media_areas: None,
            caption: Some(TextWithEntities::simple("Updated mock caption")),
            privacy: Some(StoryPrivacyInput {
                privacy: StoryPrivacy::SelectedContacts,
                allow_peers: vec![mock_peers::AURORA],
                disallow_peers: Vec::new(),
            }),
        },
        NOW_SECS + 5,
    )
    .expect("MockSession::edit_story");

    let edited = store.lookup(full_id).expect("edited story in store");
    assert_eq!(edited.caption.text, "Updated mock caption");
    assert_eq!(edited.privacy, StoryPrivacy::SelectedContacts);
    assert_eq!(edited.privacy_allowed, vec![mock_peers::AURORA]);
    assert!(edited.edited);
}

#[test]
fn open_story_deep_link_targets_exact_peer_and_story_id_with_remote_resolve() {
    // 1. Mock host: open exact story 202 on AURORA (not default first unread 201!).
    let mut mock_host = AppStoriesHost::new();
    assert!(mock_host.open_story_deep_link(mock_peers::AURORA, Some(202)));
    assert_eq!(
        mock_host.viewer().controller().shown_id(),
        Some(FullStoryId::new(mock_peers::AURORA, 202))
    );
    assert!(!mock_host.open_story_deep_link(mock_peers::AURORA, Some(999_999)));

    // 2. Live host: remote resolve via `stories.getStoriesByID` when story is not yet in store.
    let h = StoryComposeHarness::start();
    let contact_peer = h.contact_peer;
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID {
            let req = tl_fns::stories::GetStoriesByID::from_bytes(raw).ok()?;
            let stories = req
                .id
                .into_iter()
                .filter(|&sid| sid == 777)
                .map(|sid| make_photo_story_item(sid, "Resolved deep link story", false))
                .collect();
            let resp = tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                count: 1,
                stories,
                pinned_to_top: None,
                chats: Vec::new(),
                users: Vec::new(),
            });
            return Some(Ok(resp.to_bytes()));
        }
        None
    }));

    let store = StoriesStore::new(h.self_peer);
    let mut live_host = AppStoriesHost::with_backend(store, Box::new(h.live.clone()));
    assert!(live_host.open_story_deep_link(contact_peer, Some(777)));
    assert_eq!(
        live_host.viewer().controller().shown_id(),
        Some(FullStoryId::new(contact_peer, 777))
    );
    let by_id_calls = h
        .fake
        .recorded_calls_of(tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID);
    assert_eq!(by_id_calls.len(), 1);
    let req = tl_fns::stories::GetStoriesByID::from_bytes(&by_id_calls[0]).expect("decode");
    assert_eq!(req.id, vec![777]);

    h.shutdown();
}

#[test]
fn kittest_strip_add_story_opens_composer_edits_privacy_and_posts_live_story() {
    let h = StoryComposeHarness::start();
    let self_uid = h.self_peer.to_user().expect("self user").0 as i64;
    h.fake.set_dynamic_handler(Arc::new(move |ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::stories::SendStory::CONSTRUCTOR_ID {
            let updates = tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::Story(tl_types::UpdateStory {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: self_uid }),
                    story: make_photo_story_item(812, "Bold sunrise", false),
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 1,
            });
            return Some(Ok(updates.to_bytes()));
        }
        None
    }));

    struct UiState {
        host: AppStoriesHost,
        palette: Palette,
    }

    let store = StoriesStore::new(h.self_peer);
    let state = UiState {
        host: AppStoriesHost::with_backend(store, Box::new(h.live.clone())),
        palette: Palette::day(),
    };

    let mut harness = Harness::builder()
        .with_size(Vec2::new(760.0, 700.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut UiState| {
                egui::Panel::top("stories_strip_panel").show(ui, |ui| {
                    st.host.show_strip(ui, &st.palette);
                });
                st.host.update_and_render(ui.ctx(), &st.palette);
            },
            state,
        );

    harness.step();
    // 1. Click "+ Add Story" button on StoriesStrip -> opens SendStoryBox.
    harness.get_by_label("+ Add Story").click();
    harness.step();
    harness.step();
    assert!(
        harness.state().host.composer().is_some(),
        "clicking '+ Add Story' must open SendStoryBox"
    );

    // 2. Pick sample photo, type formatted caption, pick 12h period, disallow forwards.
    harness.get_by_label("Sample Photo").click();
    harness.step();
    harness.step();
    {
        let comp = harness
            .state_mut()
            .host
            .composer_mut()
            .expect("composer open");
        assert!(
            comp.media.is_some(),
            "Sample Photo button must attach media"
        );
        comp.caption_text = "**Bold** sunrise".to_owned();
        comp.allow_forwards = false;
    }
    harness.get_by_label("12h").click();
    harness.step();
    harness.step();

    // 3. Open StoryPrivacyBox -> choose My Contacts -> Exclude CedarContact -> Save Privacy.
    harness.get_by_label("Edit Privacy...").click();
    harness.step();
    harness.step();
    harness.get_by_label("My Contacts").click();
    harness.step();
    harness.step();
    let exclude_btns: Vec<_> = harness.get_all_by_label("Exclude...").collect();
    assert!(exclude_btns.len() >= 2);
    exclude_btns[1].click();
    harness.step();
    harness.step();
    harness.get_by_label("CedarContact").click();
    harness.step();
    harness.step();
    harness.get_by_label("Done").click();
    harness.step();
    harness.step();
    harness.get_by_label("Save Privacy").click();
    harness.step();
    harness.step();

    // 4. Add a Reaction interactive media area and click "Post Story".
    harness.get_by_label("Add Sticker / Area").click();
    harness.step();
    harness.step();
    harness.get_by_label("Add Reaction").click();
    harness.step();
    harness.step();
    harness.get_by_label("Post Story").click();
    harness.step();
    harness.step();

    assert!(
        harness.state().host.composer().is_none(),
        "composer should close after successful story post"
    );
    let send_calls = h
        .fake
        .recorded_calls_of(tl_fns::stories::SendStory::CONSTRUCTOR_ID);
    assert_eq!(send_calls.len(), 1);
    let req = tl_fns::stories::SendStory::from_bytes(&send_calls[0]).expect("decode SendStory");
    assert_eq!(req.period, Some(43_200));
    assert!(req.pinned);
    assert!(req.noforwards);
    assert_eq!(req.caption.as_deref(), Some("Bold sunrise"));
    assert_eq!(req.entities.as_ref().map(Vec::len), Some(1));
    assert!(req.privacy_rules.iter().any(|r| matches!(
        r,
        tl_enums::InputPrivacyRule::InputPrivacyValueAllowContacts
    )));
    assert!(req.privacy_rules.iter().any(|r| matches!(
        r,
        tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(u) if u.users.len() == 1
    )));
    assert_eq!(req.media_areas.as_ref().map(Vec::len), Some(1));

    h.shutdown();
}

#[test]
fn kittest_story_drafts_save_resume_delete_and_upload_progress_strip() {
    struct UiState {
        host: AppStoriesHost,
        palette: Palette,
    }

    let mut mock = MockStoriesBackend::new(NOW_SECS);
    let self_id = mock.self_peer_id();
    let mut store = StoriesStore::new(self_id);
    mock.bootstrap_store(&mut store, NOW_SECS);
    mock.set_pending_uploads(vec![StoryUploadStatus {
        peer_id: self_id,
        peer_name: "My Story".to_owned(),
        progress: 0.65,
        failed: false,
    }]);

    let state = UiState {
        host: AppStoriesHost::with_backend(store, Box::new(mock)),
        palette: Palette::day(),
    };

    let mut harness = Harness::builder()
        .with_size(Vec2::new(760.0, 700.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut UiState| {
                egui::Panel::top("stories_strip_panel").show(ui, |ui| {
                    st.host.show_strip(ui, &st.palette);
                });
                st.host.update_and_render(ui.ctx(), &st.palette);
            },
            state,
        );

    harness.step();
    // Open composer via "+ Add Story", attach sample photo, write caption, save draft.
    harness.get_by_label("+ Add Story").click();
    harness.step();
    harness.step();
    harness.get_by_label("Sample Photo").click();
    harness.step();
    harness.step();
    harness
        .state_mut()
        .host
        .composer_mut()
        .expect("composer open")
        .caption_text = "Draft sunset note".to_owned();
    harness.step();
    harness.get_by_label("Save Draft").click();
    harness.step();
    harness.step();

    assert!(harness.state().host.composer().is_none());
    assert_eq!(harness.state().host.drafts().len(), 1);
    assert_eq!(
        harness.state().host.drafts()[0].caption.text,
        "Draft sunset note"
    );

    // Re-open composer -> click "Drafts (1)" -> click "Resume" in StoryDraftsBox.
    harness.get_by_label("+ Add Story").click();
    harness.step();
    harness.step();
    harness.get_by_label("Drafts (1)").click();
    harness.step();
    harness.step();
    assert!(harness.state().host.drafts_box().is_some());
    harness.get_by_label("Resume").click();
    harness.step();
    harness.step();

    let resumed = harness.state().host.composer().expect("resumed composer");
    assert_eq!(resumed.caption_text, "Draft sunset note");
    assert!(resumed.media.is_some());

    // Open drafts again and click Delete.
    harness.get_by_label("Drafts (1)").click();
    harness.step();
    harness.step();
    harness.get_by_label("Delete").click();
    harness.step();
    harness.step();
    assert!(harness.state().host.drafts().is_empty());
}

#[test]
fn kittest_edit_story_composer_submits_edit_story_rpc() {
    let h = StoryComposeHarness::start();
    let self_uid = h.self_peer.to_user().expect("self user").0 as i64;
    h.fake.set_dynamic_handler(Arc::new(move |ctor, _raw| {
        if ctor == tl_fns::stories::EditStory::CONSTRUCTOR_ID {
            let updates = tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::Story(tl_types::UpdateStory {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: self_uid }),
                    story: make_photo_story_item(905, "Edited via UI", true),
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS + 5,
                seq: 3,
            });
            return Some(Ok(updates.to_bytes()));
        }
        None
    }));

    let mut store = StoriesStore::new(h.self_peer);
    let _ = store.apply_single_tl(
        h.self_peer,
        &make_photo_story_item(905, "Original caption", false),
        NOW_SECS,
    );
    let mut host = AppStoriesHost::with_backend(store, Box::new(h.live.clone()));
    assert!(host.open_composer_for_edit(FullStoryId::new(h.self_peer, 905)));
    assert_eq!(
        host.composer().expect("edit composer").caption_text,
        "Original caption"
    );

    struct UiState {
        host: AppStoriesHost,
        palette: Palette,
    }
    let state = UiState {
        host,
        palette: Palette::day(),
    };

    let mut harness = Harness::builder()
        .with_size(Vec2::new(760.0, 700.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut UiState| {
                st.host.update_and_render(ui.ctx(), &st.palette);
            },
            state,
        );

    harness.step();
    harness
        .state_mut()
        .host
        .composer_mut()
        .expect("edit composer")
        .caption_text = "Edited via UI".to_owned();
    harness.get_by_label("Save Changes").click();
    harness.step();

    assert!(harness.state().host.composer().is_none());
    let calls = h
        .fake
        .recorded_calls_of(tl_fns::stories::EditStory::CONSTRUCTOR_ID);
    assert_eq!(calls.len(), 1);
    let req = tl_fns::stories::EditStory::from_bytes(&calls[0]).expect("decode EditStory");
    assert_eq!(req.id, 905);
    assert_eq!(req.caption.as_deref(), Some("Edited via UI"));
    assert_eq!(req.entities, Some(Vec::new()));
    assert!(req.media.is_none());

    // Sanity-check standalone SendStoryBox::new helper too.
    let _ = SendStoryBox::new(h.self_peer, &[]);

    h.shutdown();
}

#[test]
fn kittest_privacy_cancel_reverts_and_caption_limit_blocks_submit() {
    struct UiState {
        host: AppStoriesHost,
        palette: Palette,
    }

    let mut mock = MockStoriesBackend::new(NOW_SECS);
    mock.set_account_premium(false);
    let self_id = mock.self_peer_id();
    let mut store = StoriesStore::new(self_id);
    mock.bootstrap_store(&mut store, NOW_SECS);

    let mut host = AppStoriesHost::with_backend(store, Box::new(mock));
    host.open_composer();

    let state = UiState {
        host,
        palette: Palette::day(),
    };

    let mut harness = Harness::builder()
        .with_size(Vec2::new(760.0, 700.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut UiState| {
                st.host.update_and_render(ui.ctx(), &st.palette);
            },
            state,
        );

    harness.step();
    // Open privacy editor, change selection to Close Friends, then Cancel -> must revert to Public.
    harness.get_by_label("Edit Privacy...").click();
    harness.step();
    harness.step();
    harness.get_by_label("Close Friends").click();
    harness.step();
    harness.get_by_label("Cancel").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness
            .state()
            .host
            .composer()
            .expect("composer stays open")
            .privacy_box
            .privacy,
        StoryPrivacy::Public
    );

    // Non-premium caption over STORY_CAPTION_LIMIT_DEFAULT blocks submission with error_message.
    harness.get_by_label("Sample Photo").click();
    harness.step();
    harness.step();
    harness
        .state_mut()
        .host
        .composer_mut()
        .expect("composer open")
        .caption_text = "x".repeat(STORY_CAPTION_LIMIT_DEFAULT + 5);
    harness.step();
    harness.get_by_label("Post Story").click();
    harness.step();
    harness.step();
    let comp = harness
        .state()
        .host
        .composer()
        .expect("composer must stay open when caption exceeds limit");
    assert!(
        comp.error_message
            .as_deref()
            .is_some_and(|e| e.contains("Caption is too long")),
        "expected caption-limit error, got {:?}",
        comp.error_message
    );
}

#[test]
fn multi_account_open_story_deep_link_targets_each_account_independently() {
    let fake_a = FakeRpcClient::new();
    let fake_b = FakeRpcClient::new();

    let self_a_uid = 31_001_i64;
    let peer_a_uid = 41_001_i64;
    let self_b_uid = 32_002_i64;
    let peer_b_uid = 42_002_i64;

    fake_a.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID {
            let resolved =
                tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
                    peer: tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_a_uid,
                    }),
                    chats: Vec::new(),
                    users: vec![tl_enums::User::User(make_tl_user(
                        peer_a_uid,
                        "PeerAlpha",
                        "peer_alpha",
                        false,
                    ))],
                });
            return Some(Ok(resolved.to_bytes()));
        }
        if ctor == tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID {
            let req = tl_fns::stories::GetStoriesByID::from_bytes(raw).ok()?;
            let stories = req
                .id
                .into_iter()
                .filter(|&sid| sid == 501)
                .map(|sid| make_photo_story_item(sid, "Account A story 501", false))
                .collect();
            let resp = tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                count: 1,
                stories,
                pinned_to_top: None,
                chats: Vec::new(),
                users: Vec::new(),
            });
            return Some(Ok(resp.to_bytes()));
        }
        None
    }));

    fake_b.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID {
            let resolved =
                tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
                    peer: tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: peer_b_uid,
                    }),
                    chats: Vec::new(),
                    users: vec![tl_enums::User::User(make_tl_user(
                        peer_b_uid,
                        "PeerBeta",
                        "peer_beta",
                        false,
                    ))],
                });
            return Some(Ok(resolved.to_bytes()));
        }
        if ctor == tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID {
            let req = tl_fns::stories::GetStoriesByID::from_bytes(raw).ok()?;
            let stories = req
                .id
                .into_iter()
                .filter(|&sid| sid == 602)
                .map(|sid| make_photo_story_item(sid, "Account B story 602", false))
                .collect();
            let resp = tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                count: 1,
                stories,
                pinned_to_top: None,
                chats: Vec::new(),
                users: Vec::new(),
            });
            return Some(Ok(resp.to_bytes()));
        }
        None
    }));

    let start_runtime = |self_uid: i64, name: &str, fake: FakeRpcClient| {
        let cfg = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        };
        let rt = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                tg_app::live::ErasedRpcClient::new(fake),
                cfg,
            )
            .expect("start account runtime"),
        );
        rt.tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, rt.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("ready");
        rt.tokio_handle().block_on(async {
            let mut s = rt.session().write().await;
            tg_api::convert::apply_tl_users_and_chats(
                &mut s,
                &[tl_enums::User::User(make_tl_user(
                    self_uid,
                    name,
                    &name.to_lowercase(),
                    true,
                ))],
                &[],
            );
        });
        rt
    };

    let rt_a = start_runtime(self_a_uid, "AcctAlpha", fake_a.clone());
    let rt_b = start_runtime(self_b_uid, "AcctBeta", fake_b.clone());

    let temp = tempfile::TempDir::new().expect("temp dir");
    let settings_path = temp.path().join("multi_story_settings.json");
    let system = NotificationSystem::headless(
        NotificationPreferences::default(),
        NotifySettingsStore::default(),
        temp.path().join("userpics"),
    );
    let mut host = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        settings_path,
        system,
        Box::new(MockTrayPlatform::default()),
    );
    host.add_account(
        0,
        DataUserId::from_tl(self_a_uid),
        false,
        false,
        Arc::clone(&rt_a),
        None,
        true,
        false,
    );
    host.add_account(
        1,
        DataUserId::from_tl(self_b_uid),
        false,
        false,
        Arc::clone(&rt_b),
        None,
        false,
        false,
    );

    struct MultiState {
        host: MultiAccountWorkspaceHost,
        pending_url: Option<String>,
    }

    let mut harness = Harness::builder()
        .with_size(Vec2::new(1120.0, 740.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut MultiState| {
                let ctx = ui.ctx().clone();
                if let Some(url) = st.pending_url.take()
                    && let Some(ws) = st.host.active_workspace_mut()
                {
                    ws.controller_mut().handle_incoming_activation(
                        IncomingActivation {
                            urls: vec![url],
                            ..IncomingActivation::default()
                        },
                        &ctx,
                    );
                }
                let _ = st.host.show(&ctx, ui);
            },
            MultiState {
                host,
                pending_url: None,
            },
        );

    harness.step();

    // 1. Open story 501 on Account 0 (active) via deep link.
    harness.state_mut().pending_url = Some("tg://resolve?domain=peer_alpha&story=501".to_owned());
    harness.step();
    harness.step();

    let peer_a = DataPeerId::from_user(DataUserId::from_tl(peer_a_uid));
    let peer_b = DataPeerId::from_user(DataUserId::from_tl(peer_b_uid));
    assert_eq!(
        harness
            .state()
            .host
            .account(0)
            .expect("acct 0")
            .workspace
            .stories_host()
            .viewer()
            .controller()
            .shown_id(),
        Some(FullStoryId::new(peer_a, 501)),
        "Account 0 must open exact story 501"
    );
    assert!(
        !harness
            .state()
            .host
            .account(1)
            .expect("acct 1")
            .workspace
            .stories_host()
            .viewer()
            .is_open(),
        "Account 1 stories viewer must remain untouched when Account 0 opens a story"
    );
    assert_eq!(
        fake_b
            .recorded_calls_of(tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID)
            .len(),
        0,
        "Account 1 RPC must not be called for Account 0's story deep link"
    );

    // 2. Switch to Account 1 and open story 602 independently.
    assert!(harness.state_mut().host.switch_to(1));
    harness.state_mut().pending_url = Some("tg://resolve?domain=peer_beta&story=602".to_owned());
    harness.step();
    harness.step();

    assert_eq!(
        harness
            .state()
            .host
            .account(1)
            .expect("acct 1")
            .workspace
            .stories_host()
            .viewer()
            .controller()
            .shown_id(),
        Some(FullStoryId::new(peer_b, 602)),
        "Account 1 must open exact story 602 on its own stories_host"
    );
    assert_eq!(
        harness
            .state()
            .host
            .account(0)
            .expect("acct 0")
            .workspace
            .stories_host()
            .viewer()
            .controller()
            .shown_id(),
        Some(FullStoryId::new(peer_a, 501)),
        "Account 0's stories viewer state must stay isolated from Account 1"
    );

    // 3. Unavailable/expired story on Account 1 fails gracefully without switching shown story.
    harness.state_mut().pending_url = Some("tg://resolve?domain=peer_beta&story=999".to_owned());
    harness.step();
    harness.step();
    assert_eq!(
        harness
            .state()
            .host
            .account(1)
            .expect("acct 1")
            .workspace
            .stories_host()
            .viewer()
            .controller()
            .shown_id(),
        Some(FullStoryId::new(peer_b, 602)),
        "Unavailable story must not replace the active story on Account 1"
    );

    drop(harness);
    if let Ok(rt) = Arc::try_unwrap(rt_a) {
        rt.shutdown();
    }
    if let Ok(rt) = Arc::try_unwrap(rt_b) {
        rt.shutdown();
    }
}
