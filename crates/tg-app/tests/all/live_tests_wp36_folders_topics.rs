//! WP-36 live fake-RPC & UI tests (`FakeRpcClient` only, synthetic data).
//!
//! Covers:
//! - Multi-link folder invite manager (`chatlists.{get,edit,delete}ExportedInvite`,
//!   `chatlists.{get,join}ChatlistUpdates`) + `FolderLinksBoxState` UI
//! - Folder color tags (`messages.toggleDialogFilterTags`) + `render_folders_section` UI
//! - Custom folder icon picker (`ui/filter_icon_panel`, 30 icons + `emoticon` wire sync)
//! - Forum topic default icons (`data_forum_icons` / `messages.getStickerSet(EmojiDefaultTopicIcons)`)
//! - Subsection tabs slider (`SubsectionTabsWidget`, Top/Left/Bottom & appearance bit-packing)
//! - Inline self-forwards tagger (`SelfForwardsTaggerState`, `messages.sendReaction`,
//!   `messages.updateSavedReactionTag`)
//! - Peer member rank/tag editor (`EditTagControl` / `EditPeerTagBoxState`,
//!   `messages.editChatParticipantRank`)

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    Channel, ChannelFlags, ChannelId, PeerId as DataPeerId, PeerUpdateFlags, User,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::boxes::peer_tags::{
    BadgeRole, EditPeerTagBoxState, MAX_PEER_TAG_RANK_LEN, sanitize_peer_tag_rank,
};
use tg_app::history::backend::{HistoryBackend, SavedReactionTagItem};
use tg_app::history::sections_model::{
    FORUM_GENERAL_TOPIC_ID, ForumTopic, SubsectionTabsAppearance, SubsectionTabsMode,
    pack_subsection_tabs_state, unpack_subsection_tabs_state,
};
use tg_app::history::sections_ui::{
    HistorySectionAction, HistorySectionKind, SelfForwardsTaggerAction, SelfForwardsTaggerPhase,
    SelfForwardsTaggerState, SubsectionTabsWidget,
};
use tg_app::history::theme::HistoryColors;
use tg_app::info::backend::{
    InfoBackend, InfoPeerKind, MemberEntry, MemberRole, PeerProfile, SharedMediaCounts,
};
use tg_app::live::LiveSession;
use tg_app::live::convert::from_data_peer;
use tg_app::settings::backend::{ChatFolder, FolderIcon, SettingsBackend};
use tg_app::settings::folder_links::{FolderLinksBoxState, FolderLinksView};
use tg_app::settings::folders::{FoldersState, render_filter_icon_panel, render_folders_section};
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{MsgId, PeerId};

// =============================================================================
// Synthetic FakeRpcClient & Live Test Harness
// =============================================================================

type RecordedCall = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
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

    fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn last_request<R: Deserializable + Identifiable>(&self) -> R {
        let calls = self.recorded_calls();
        let (_, bytes) = calls
            .iter()
            .rev()
            .find(|(ctor, _)| *ctor == R::CONSTRUCTOR_ID)
            .expect("expected matching RPC call to be recorded");
        R::from_bytes(bytes).expect("deserialize recorded TL request")
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = if bytes.len() >= 4 {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            0
        };
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected_ctor, res)) => {
                assert_eq!(
                    ctor, expected_ctor,
                    "unexpected RPC constructor: got 0x{ctor:08x}, expected 0x{expected_ctor:08x}"
                );
                let raw = res?;
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Rpc(tg_api::RpcError {
                code: 500,
                name: format!("UNEXPECTED_RPC_0x{ctor:08X}"),
                value: None,
            })),
        }
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
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

struct Wp36Harness {
    fake: FakeRpcClient,
    live: LiveSession,
    self_peer: PeerId,
    bob_peer: PeerId,
    bob_uid: i64,
    group_peer: PeerId,
    group_cid: i64,
}

impl Wp36Harness {
    fn start() -> Self {
        let fake = FakeRpcClient::new();
        let self_uid = 700_001_u64;
        let bob_uid = 700_002_i64;
        let group_cid = 800_001_i64;

        let cfg = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        };
        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId(self_uid),
                tg_app::live::ErasedRpcClient::new(fake.clone()),
                cfg,
            )
            .expect("start SessionRuntime"),
        );

        let session_arc = Arc::clone(runtime.session());
        runtime.tokio_handle().block_on(async move {
            let mut s = session_arc.write().await;

            let mut me = User::new(DataUserId(self_uid), true);
            me.first_name = "Self".to_owned();
            me.common.name = "Self Tester".to_owned();
            me.access_hash = 111;
            s.upsert_user(me, PeerUpdateFlags::NAME);

            let mut bob = User::new(DataUserId(bob_uid as u64), false);
            bob.first_name = "Sample".to_owned();
            bob.common.name = "Sample Member".to_owned();
            bob.access_hash = 222;
            s.upsert_user(bob, PeerUpdateFlags::NAME);

            let mut grp = Channel::new(ChannelId(group_cid as u64));
            grp.common.name = "Sample Forum".to_owned();
            grp.access_hash = 333;
            grp.flags |= ChannelFlags::MEGAGROUP | ChannelFlags::FORUM;
            s.upsert_channel(grp, PeerUpdateFlags::NAME);
        });

        let live = LiveSession::new(runtime, 36);
        Self {
            fake,
            live,
            self_peer: from_data_peer(DataPeerId::from_user(DataUserId(self_uid))),
            bob_peer: from_data_peer(DataPeerId::from_user(DataUserId(bob_uid as u64))),
            bob_uid,
            group_peer: from_data_peer(DataPeerId::from_channel(ChannelId(group_cid as u64))),
            group_cid,
        }
    }
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_600,
        seq: 1,
    })
}

// =============================================================================
// 1. Multi-Link Folder Invite CRUD + Updates (`chatlists.*`)
// =============================================================================

#[test]
fn wp36_chatlists_multi_link_crud_and_updates() {
    let mut h = Wp36Harness::start();
    let bob_raw = DataPeerId::from_user(DataUserId(h.bob_uid as u64)).0;
    let grp_raw = DataPeerId::from_channel(ChannelId(h.group_cid as u64)).0;

    // 1. `chatlists.getExportedInvites` returns 2 existing links.
    let get_resp = tl_enums::chatlists::ExportedInvites::ExportedInvites(
        tl_types::chatlists::ExportedInvites {
            invites: vec![
                tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(
                    tl_types::ExportedChatlistInvite {
                        title: "Core Team".to_owned(),
                        url: "https://t.me/addlist/coreSlug1".to_owned(),
                        peers: vec![tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: h.group_cid,
                        })],
                    },
                ),
                tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(
                    tl_types::ExportedChatlistInvite {
                        title: "Guests".to_owned(),
                        url: "https://t.me/addlist/guestSlug2".to_owned(),
                        peers: vec![tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: h.bob_uid,
                        })],
                    },
                ),
            ],
            chats: Vec::new(),
            users: Vec::new(),
        },
    );
    h.fake.enqueue_ok(
        tl_fns::chatlists::GetExportedInvites::CONSTRUCTOR_ID,
        &get_resp,
    );

    let links = SettingsBackend::folder_invite_links(&h.live, 2);
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].slug, "coreSlug1");
    assert_eq!(links[0].title, "Core Team");
    assert_eq!(links[1].slug, "guestSlug2");

    // 2. `chatlists.editExportedInvite` updates title and peers.
    let edit_resp = tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(
        tl_types::ExportedChatlistInvite {
            title: "Engineering Core".to_owned(),
            url: "https://t.me/addlist/coreSlug1".to_owned(),
            peers: vec![
                tl_enums::Peer::Channel(tl_types::PeerChannel {
                    channel_id: h.group_cid,
                }),
                tl_enums::Peer::User(tl_types::PeerUser { user_id: h.bob_uid }),
            ],
        },
    );
    h.fake.enqueue_ok(
        tl_fns::chatlists::EditExportedInvite::CONSTRUCTOR_ID,
        &edit_resp,
    );

    let edited = SettingsBackend::edit_folder_invite_link(
        &mut h.live,
        2,
        "coreSlug1",
        Some("Engineering Core"),
        Some(&[grp_raw, bob_raw]),
    )
    .expect("edit_folder_invite_link");
    assert_eq!(edited.title, "Engineering Core");
    assert_eq!(edited.peers.len(), 2);

    let edit_req: tl_fns::chatlists::EditExportedInvite = h.fake.last_request();
    assert_eq!(edit_req.slug, "coreSlug1");
    assert_eq!(edit_req.title.as_deref(), Some("Engineering Core"));
    assert_eq!(edit_req.peers.as_ref().map(Vec::len), Some(2));

    // 3. `chatlists.deleteExportedInvite` removes the second link.
    h.fake.enqueue_ok(
        tl_fns::chatlists::DeleteExportedInvite::CONSTRUCTOR_ID,
        &true,
    );
    let deleted = SettingsBackend::delete_folder_invite_link(&mut h.live, 2, "guestSlug2")
        .expect("delete_folder_invite_link");
    assert!(deleted);
    assert_eq!(h.live.cached_chatlist_links(2).len(), 1);
    assert_eq!(h.live.cached_chatlist_links(2)[0].slug, "coreSlug1");

    // 4. `chatlists.getChatlistUpdates` + `chatlists.joinChatlistUpdates` via SettingsBackend.
    let upd_resp = tl_enums::chatlists::ChatlistUpdates::ChatlistUpdates(
        tl_types::chatlists::ChatlistUpdates {
            missing_peers: vec![tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: h.group_cid,
            })],
            chats: Vec::new(),
            users: Vec::new(),
        },
    );
    h.fake.enqueue_ok(
        tl_fns::chatlists::GetChatlistUpdates::CONSTRUCTOR_ID,
        &upd_resp,
    );
    let missing = SettingsBackend::check_folder_updates(&mut h.live, 2).expect("check updates");
    assert_eq!(missing, vec![grp_raw]);

    h.fake.enqueue_ok(
        tl_fns::chatlists::JoinChatlistUpdates::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    SettingsBackend::join_folder_updates(&mut h.live, 2, &missing).expect("join updates");
    let join_req: tl_fns::chatlists::JoinChatlistUpdates = h.fake.last_request();
    assert_eq!(join_req.peers.len(), 1);
}

// =============================================================================
// 2. Folder Color Tags (`messages.toggleDialogFilterTags`) + Custom Folder Icon
// =============================================================================

#[test]
fn wp36_toggle_dialog_filter_tags_and_custom_folder_icon() {
    let mut h = Wp36Harness::start();

    // Initial `messages.getDialogFilters` reports `tags_enabled: false`.
    let filters_resp =
        tl_enums::messages::DialogFilters::DialogFilters(tl_types::messages::DialogFilters {
            tags_enabled: false,
            filters: Vec::new(),
        });
    h.fake.enqueue_ok(
        tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID,
        &filters_resp,
    );
    assert!(!SettingsBackend::folder_tags_enabled(&h.live));

    // Toggle tags ON via `messages.toggleDialogFilterTags`.
    h.fake.enqueue_ok(
        tl_fns::messages::ToggleDialogFilterTags::CONSTRUCTOR_ID,
        &true,
    );
    assert!(
        SettingsBackend::set_folder_tags_enabled(&mut h.live, true)
            .expect("toggleDialogFilterTags(true)")
    );
    assert!(SettingsBackend::folder_tags_enabled(&h.live));
    let toggle_req: tl_fns::messages::ToggleDialogFilterTags = h.fake.last_request();
    assert!(toggle_req.enabled);

    // Save a folder with a custom `FilterIconPanel` icon (`FolderIcon::Cat`) & color tag (`Some(2)`).
    h.fake
        .enqueue_ok(tl_fns::messages::UpdateDialogFilter::CONSTRUCTOR_ID, &true);
    let draft = ChatFolder {
        id: 0,
        title: "Kitties".to_owned(),
        icon: FolderIcon::Cat,
        color_index: Some(2),
        include_groups: true,
        ..ChatFolder::default()
    };
    let saved = SettingsBackend::save_folder(&mut h.live, draft).expect("save custom icon folder");
    assert_eq!(saved.icon, FolderIcon::Cat);
    assert_eq!(saved.color_index, Some(2));

    let upd_req: tl_fns::messages::UpdateDialogFilter = h.fake.last_request();
    match upd_req.filter.expect("filter present") {
        tl_enums::DialogFilter::DialogFilter(df) => {
            assert_eq!(df.emoticon.as_deref(), Some(FolderIcon::Cat.emoticon()));
            assert_eq!(df.color, Some(2));
        }
        other => panic!("expected DialogFilter, got {other:?}"),
    }

    // Round-trip via `SettingsBackend::folders()` preserves `FolderIcon::Cat` and `color_index`.
    let loaded = SettingsBackend::folders(&h.live);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].icon, FolderIcon::Cat);
    assert_eq!(loaded[0].color_index, Some(2));

    // Verify all 30 `FilterIconPanel` icons round-trip through `emoticon()` / `from_emoticon()`.
    assert_eq!(FolderIcon::ALL.len(), 30);
    for icon in FolderIcon::ALL {
        assert_eq!(
            FolderIcon::from_emoticon(icon.emoticon()),
            Some(icon),
            "emoticon round-trip failed for {icon:?}"
        );
    }
}

// =============================================================================
// 3. Forum Topic Custom Icons (`Data::ForumIcons` / `getForumTopicIcons`)
// =============================================================================

#[test]
fn wp36_forum_topic_icons_rpc_and_cache() {
    let h = Wp36Harness::start();

    let sticker_set_resp =
        tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                archived: false,
                official: true,
                masks: false,
                emojis: true,
                text_color: false,
                channel_emoji_status: false,
                creator: false,
                installed_date: None,
                id: 99_001,
                access_hash: 12345,
                title: "Default Topic Icons".to_owned(),
                short_name: "topic_icons".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 2,
                hash: 42,
            }),
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![
                tl_enums::Document::Document(tl_types::Document {
                    id: 5309984423003823246,
                    access_hash: 101,
                    file_reference: vec![1, 2, 3],
                    date: 1_795_341_600,
                    mime_type: "application/x-tgsticker".to_owned(),
                    size: 1024,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl_enums::DocumentAttribute::CustomEmoji(
                        tl_types::DocumentAttributeCustomEmoji {
                            free: true,
                            text_color: false,
                            alt: "💡".to_owned(),
                            stickerset: tl_enums::InputStickerSet::EmojiDefaultTopicIcons,
                        },
                    )],
                }),
                tl_enums::Document::Document(tl_types::Document {
                    id: 5310023225525163645,
                    access_hash: 102,
                    file_reference: vec![4, 5, 6],
                    date: 1_795_341_600,
                    mime_type: "image/webp".to_owned(),
                    size: 512,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl_enums::DocumentAttribute::Sticker(
                        tl_types::DocumentAttributeSticker {
                            mask: false,
                            alt: "📢".to_owned(),
                            stickerset: tl_enums::InputStickerSet::EmojiDefaultTopicIcons,
                            mask_coords: None,
                        },
                    )],
                }),
            ],
        });
    h.fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &sticker_set_resp,
    );

    let icons = h
        .live
        .get_forum_topic_icons()
        .expect("get_forum_topic_icons");
    assert_eq!(icons.len(), 2);
    assert_eq!(icons[0].document_id, 5309984423003823246);
    assert_eq!(icons[0].alt_emoji, "💡");
    assert!(icons[0].is_animated);
    assert_eq!(icons[1].document_id, 5310023225525163645);
    assert_eq!(icons[1].alt_emoji, "📢");
    assert!(!icons[1].is_animated);

    let req: tl_fns::messages::GetStickerSet = h.fake.last_request();
    assert!(matches!(
        req.stickerset,
        tl_enums::InputStickerSet::EmojiDefaultTopicIcons
    ));
    assert_eq!(req.hash, 0);

    // Subsequent call through `HistoryBackend::forum_topic_icons` hits cache (no extra RPC).
    let cached = HistoryBackend::forum_topic_icons(&h.live);
    assert_eq!(cached.len(), 2);
    assert_eq!(h.fake.recorded_calls().len(), 1);
}

// =============================================================================
// 4. Inline Self-Forwards Tagger (`messages.sendReaction` & `updateSavedReactionTag`)
// =============================================================================

#[test]
fn wp36_self_forwards_tagger_rpcs_and_state() {
    let mut h = Wp36Harness::start();

    // 1. Tagging 2 self-forwarded messages sends `messages.sendReaction` for each ID.
    h.fake.enqueue_ok(
        tl_fns::messages::SendReaction::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::SendReaction::CONSTRUCTOR_ID,
        &empty_updates(),
    );

    let tagged = h
        .live
        .tag_self_forwarded_messages(h.self_peer, &[MsgId(101), MsgId(102)], "🔥")
        .expect("tag_self_forwarded_messages");
    assert_eq!(tagged, 2);

    let last_react: tl_fns::messages::SendReaction = h.fake.last_request();
    assert_eq!(last_react.msg_id, 102);

    // 2. Renaming a saved reaction tag sends `messages.updateSavedReactionTag`.
    h.fake.enqueue_ok(
        tl_fns::messages::UpdateSavedReactionTag::CONSTRUCTOR_ID,
        &true,
    );
    let updated = HistoryBackend::update_saved_reaction_tag(&mut h.live, "🔥", Some("Urgent"))
        .expect("update_saved_reaction_tag");
    assert!(updated);
    let tag_req: tl_fns::messages::UpdateSavedReactionTag = h.fake.last_request();
    assert_eq!(tag_req.title.as_deref(), Some("Urgent"));
    assert!(matches!(
        tag_req.reaction,
        tl_enums::Reaction::Emoji(ref e) if e.emoticon == "🔥"
    ));

    // 3. Tagger UI state machine + `egui_kittest` click emits `ApplyTag`.
    let mut tagger = SelfForwardsTaggerState::default();
    tagger.show_for_forwarded(h.self_peer, vec![MsgId(101), MsgId(102)], 10.0);
    assert!(matches!(
        tagger.phase,
        Some(SelfForwardsTaggerPhase::SelectingTag { .. })
    ));

    let colors = HistoryColors::from_palette(&Palette::day());
    let tags = vec![SavedReactionTagItem {
        emoji: "🔥".to_owned(),
        title: Some("Urgent".to_owned()),
        count: 3,
    }];
    let captured_action: Arc<Mutex<Option<SelfForwardsTaggerAction>>> = Arc::new(Mutex::new(None));
    let cap_clone = Arc::clone(&captured_action);

    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(640.0, 320.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
        })
        .build_ui(move |ui| {
            let rect = ui.max_rect();
            if let Some(act) = tagger.show(ui, rect, &tags, 10.5, &colors) {
                *cap_clone.lock().expect("lock action") = Some(act);
            }
        });
    harness.run();
    use egui_kittest::kittest::Queryable;
    harness.get_by_label("🔥 Urgent").click();
    harness.run();

    let act = captured_action
        .lock()
        .expect("lock action")
        .clone()
        .expect("expected ApplyTag action");
    assert_eq!(
        act,
        SelfForwardsTaggerAction::ApplyTag {
            saved_peer_id: h.self_peer,
            msg_ids: vec![MsgId(101), MsgId(102)],
            emoji: "🔥".to_owned(),
        }
    );
}

// =============================================================================
// 5. Peer Member Rank / Tag Editor (`EditTagControl` & `messages.editChatParticipantRank`)
// =============================================================================

#[test]
fn wp36_peer_rank_tag_sanitization_and_rpc() {
    let mut h = Wp36Harness::start();

    // Verify emoji stripping, newline flattening, and 16-char cap (`kRankLimit = 16`).
    assert_eq!(sanitize_peer_tag_rank("🔥 Core\nDev 🚀"), "Core Dev");
    assert_eq!(
        sanitize_peer_tag_rank("VeryLongModeratorTitleExceedingLimit")
            .chars()
            .count(),
        MAX_PEER_TAG_RANK_LEN
    );

    // Invoke `messages.editChatParticipantRank` via `InfoBackend::edit_member_rank`.
    h.fake.enqueue_ok(
        tl_fns::messages::EditChatParticipantRank::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let saved = InfoBackend::edit_member_rank(&mut h.live, h.group_peer, h.bob_peer, "⭐ Lead Dev")
        .expect("edit_member_rank");
    assert_eq!(saved, "Lead Dev");
    assert_eq!(
        InfoBackend::member_rank(&h.live, h.group_peer, h.bob_peer).as_deref(),
        Some("Lead Dev")
    );

    let rank_req: tl_fns::messages::EditChatParticipantRank = h.fake.last_request();
    assert_eq!(rank_req.rank, "Lead Dev");

    // Verify `EditPeerTagBoxState` preview + submission flow.
    let profile = PeerProfile {
        id: h.group_peer,
        kind: InfoPeerKind::Supergroup,
        title: "Sample Forum".to_owned(),
        status: "10 members".to_owned(),
        is_online: false,
        verified: false,
        premium: false,
        color_index: 2,
        phone: None,
        usernames: Vec::new(),
        bio: String::new(),
        birthday: None,
        invite_link: None,
        muted: false,
        is_contact: false,
        is_blocked: false,
        can_edit: true,
        can_add_members: true,
        can_manage_admins: true,
        can_manage_permissions: true,
        is_public: false,
        no_forwards: false,
        is_forum: true,
        members_count: 10,
        online_count: 1,
        admins_count: 1,
        default_banned_rights: tg_data::rights::ChatBannedRights::empty(),
        slowmode_seconds: 0,
        auto_delete_ttl_seconds: 0,
        linked_discussion_peer: None,
        bot_commands_count: 0,
        bot_reads_all_messages: false,
        has_mini_app: false,
        media_counts: SharedMediaCounts::default(),
        group_call: Default::default(),
    };
    let member = MemberEntry {
        peer_id: h.bob_peer,
        name: "Sample Member".to_owned(),
        username: Some("sample_member".to_owned()),
        status_text: "online".to_owned(),
        is_online: true,
        is_bot: false,
        role: MemberRole::Member,
        color_index: 3,
    };

    let mut box_state =
        EditPeerTagBoxState::from_member(&profile, &member, Some("Lead Dev"), false);
    assert_eq!(box_state.control.role, BadgeRole::User);
    assert_eq!(box_state.control.effective_badge_text(), "Lead Dev");

    // Render `EditTagControl` + `EditPeerTagBoxState` cleanly in `egui_kittest`.
    let ui_colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(420.0, 320.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
        })
        .build_ui(move |ui| {
            box_state.show_body(ui, &ui_colors);
        });
    harness.run();
}

// =============================================================================
// 6. Subsection Tabs Slider & Folders UI (`egui_kittest`)
// =============================================================================

#[test]
fn wp36_subsection_tabs_slider_and_folders_ui() {
    // 1. Bit-packing round-trip matches tdesktop `history_view_subsection_tabs.cpp`.
    for mode in [
        SubsectionTabsMode::Top,
        SubsectionTabsMode::Left,
        SubsectionTabsMode::Bottom,
    ] {
        for app in [
            SubsectionTabsAppearance::TextAndIcons,
            SubsectionTabsAppearance::TextOnly,
            SubsectionTabsAppearance::IconsOnly,
        ] {
            let packed = pack_subsection_tabs_state(mode, app);
            assert_eq!(unpack_subsection_tabs_state(packed), (mode, app));
        }
    }

    // 2. `SubsectionTabsWidget` layout toggle + topic click in `egui_kittest`.
    let forum_peer = PeerId(900_001);
    let topics = vec![
        ForumTopic {
            root_id: FORUM_GENERAL_TOPIC_ID,
            peer_id: forum_peer,
            title: "General".to_owned(),
            icon_color: 0x6FB9F0,
            custom_emoji: None,
            is_general: true,
            is_pinned: true,
            is_closed: false,
            muted: false,
            unread_count: 0,
            unread_mentions: 0,
            last_sender: "Alex".to_owned(),
            last_preview: "Welcome".to_owned(),
            last_date_unix: 1_795_341_600,
            draft_preview: None,
        },
        ForumTopic {
            root_id: MsgId(42),
            peer_id: forum_peer,
            title: "Design".to_owned(),
            icon_color: 0xCB86DB,
            custom_emoji: Some("🎨".to_owned()),
            is_general: false,
            is_pinned: false,
            is_closed: false,
            muted: false,
            unread_count: 3,
            unread_mentions: 1,
            last_sender: "Riley".to_owned(),
            last_preview: "Updated mockups".to_owned(),
            last_date_unix: 1_795_341_700,
            draft_preview: None,
        },
    ];

    let colors = HistoryColors::from_palette(&Palette::day());
    let mut tabs = SubsectionTabsWidget::new();
    let mut mock_hist = tg_app::history::mock::MockHistoryStore::new();
    let captured_tab: Arc<Mutex<Option<HistorySectionAction>>> = Arc::new(Mutex::new(None));
    let cap_tab_cb = Arc::clone(&captured_tab);
    let captured_mode: Arc<Mutex<SubsectionTabsMode>> =
        Arc::new(Mutex::new(SubsectionTabsMode::Top));
    let cap_mode_cb = Arc::clone(&captured_mode);

    let mut tab_harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(640.0, 360.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
        })
        .build_ui(move |ui| {
            let outer = ui.max_rect();
            let (_rem, act) = tabs.show_for_forum(
                ui,
                outer,
                forum_peer,
                None,
                &topics,
                &mut mock_hist,
                &colors,
            );
            *cap_mode_cb.lock().expect("lock mode") = tabs.mode;
            if let Some(a) = act {
                *cap_tab_cb.lock().expect("lock tab act") = Some(a);
            }
        });
    tab_harness.run();
    use egui_kittest::kittest::Queryable;
    tab_harness
        .get_by_label("Subsection Tabs Layout (Tabs on Top)")
        .click();
    tab_harness.run();
    assert_eq!(
        *captured_mode.lock().expect("lock mode"),
        SubsectionTabsMode::Left
    );
    tab_harness.get_by_label("Design").click();
    tab_harness.run();
    assert_eq!(
        *captured_tab.lock().expect("lock tab act"),
        Some(HistorySectionAction::OpenSection(
            HistorySectionKind::ForumTopicHistory {
                peer_id: forum_peer,
                topic_root_id: MsgId(42),
            }
        ))
    );

    // 3. `render_filter_icon_panel`, `FolderLinksBoxState`, and `render_folders_section` render cleanly.
    let ui_colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
    let mut mock_settings = MockSettingsBackend::new();
    let mut folders_state = FoldersState::default();
    let mut core = tg_core::CoreSettings::default();
    let mut picked_icon = FolderIcon::Custom;
    let mut links_box = FolderLinksBoxState::new(2, "Work", true, vec![2001]);
    links_box.view = FolderLinksView::List;

    let mut ui_harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(640.0, 720.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
        })
        .build_ui(move |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let _ = render_folders_section(
                    ui,
                    &mut folders_state,
                    &mut core,
                    &mut mock_settings,
                    &ui_colors,
                );
                if let Some(ic) = render_filter_icon_panel(ui, picked_icon, &ui_colors) {
                    picked_icon = ic;
                }
                let _ = links_box.show_body(ui, &mut mock_settings, &ui_colors);
            });
        });
    ui_harness.run();
}
