//! WP-37 integration & UI tests:
//! 1. `InputStickerSetDice` Lottie pack fetching (`messages.getStickerSet` + `.tgs` download +
//!    `messages.getEmojiGameInfo` + slot-machine / face mapping) & `EmojiSetsManager`
//!    (`contacts.resolveUsername` + `channels.getMessages` + `upload.getFile` + safe ZIP unpack + UI).
//! 2. Interactive `Story`, `Giveaway`, `GiveawayResults`, and `VideoStream` chat bubbles replacing
//!    static `LinkPreviewData` cards, with `egui_kittest` click routing.
//! 3. `VideoStreamViewer` (`media_view_video_stream` + `phone.getGroupCall` /
//!    `getGroupCallStreamChannels` / `getGroupCallStreamRtmpUrl`), `PlaybackSponsoredOverlay`
//!    (`media_view_playback_sponsored`), and `.tdesktop-theme`/`.tdesktop-palette` preview inside
//!    `MediaOverlayWidget`.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

use egui::{Pos2, Rect, Vec2};
use egui_kittest::Harness;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    CallId, Channel, ChannelId, DiceData, GiveawayData, GiveawayResultsData, Message,
    MessageContent, MessageMedia, MsgId as DataMsgId, Peer, PeerId as DataPeerId, PeerUpdateFlags,
    Session, StoryMediaData, User, UserId, VideoStreamData,
};
use tg_history::BubbleRounding;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use tg_app::chat_helpers::{
    EMOJI_SETS, EmojiSetState, EmojiSetsManager, ManageEmojiSetsAction, good_emoji_set_part_name,
};
use tg_app::history::backend::{HistoryBackend, HistoryMedia};
use tg_app::history::media::{
    MediaAssetCache, MediaInteraction, VoicePlaybackState, paint_history_media,
};
use tg_app::history::sponsored::SponsoredMessageItem;
use tg_app::history::theme::HistoryColors;
use tg_app::live::convert::{
    from_data_peer, message_to_history_message, message_to_viewer_media_item,
};
use tg_app::live::dice_pack::{
    DICE_EMOJI, DicePack, SLOT_EMOJI, apply_emoji_game_info_update, cached_dice_game_options,
    fetch_dice_pack, fetch_video_stream_info, fetch_video_stream_rtmp_url, is_slot_emoji,
    normalize_dice_emoji, resolve_dice_game_options, snapshot_dice_pack,
};
use tg_app::live::session::LiveSession;
use tg_app::media::{
    MediaItem, MediaItemId, MediaItemKind, MediaOverlayWidget, MockMediaBackend, OverlayEvent,
    PlaybackSponsoredEvent, PlaybackSponsoredOverlay, SPONSORED_START_DELAY_MIN_MS,
    SponsoredForVideo, SponsoredForVideoState, VideoStreamAction, VideoStreamComment,
    VideoStreamDescriptor, VideoStreamDonor, VideoStreamQuality, VideoStreamStatus, is_theme_file,
    top_donor_places, top_video_stream_donors,
};
use tg_app::stand_in::theme::{Palette, ThemeMode};
use tg_app::stand_in::types::MsgId;
use tg_app::stories::AppStoriesHost;

// =============================================================================
// Local FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
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

    fn recorded_payloads(&self, ctor: u32) -> Vec<Vec<u8>> {
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
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
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

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, Ok(raw))) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC constructor: got {ctor:#010x}, expected {expected:#010x}"
                );
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            Some((expected, Err(err))) => {
                assert_eq!(ctor, expected);
                Err(err)
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("UNMOCKED_RPC_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }
}

fn make_ui_harness<'a, S>(
    size: Vec2,
    app: impl FnMut(&mut egui::Ui, &mut S) + 'a,
    state: S,
) -> Harness<'a, S> {
    Harness::builder()
        .with_size(size)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
        })
        .build_ui_state(app, state)
}

fn make_live_session() -> (LiveSession, FakeRpcClient, tempfile::TempDir) {
    let fake = FakeRpcClient::new(2);
    let self_id = UserId(1001);
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = SessionRuntime::start(
        self_id,
        tg_app::live::ErasedRpcClient::new(fake.clone()),
        cfg,
    )
    .expect("start runtime");
    let live = LiveSession::new(Arc::new(runtime), 37);
    let cache_dir = tempfile::tempdir().expect("tempdir for media cache");
    live.set_media_cache_dir(cache_dir.path().join("media_cache"))
        .expect("reopen media cache in TempDir");
    (live, fake, cache_dir)
}

fn make_regular_message(peer: DataPeerId, id: i32, media: MessageMedia) -> Message {
    let mut msg = Message::new_text(
        peer,
        DataMsgId(i64::from(id)),
        Some(peer),
        1_730_000_100,
        tg_data::TextWithEntities::default(),
    );
    msg.content = MessageContent::Regular {
        text: tg_data::TextWithEntities::default(),
        media,
    };
    msg
}

fn minimal_lottie_json(name: &str) -> String {
    format!(
        r#"{{"v":"5.7.4","fr":30,"ip":0,"op":30,"w":128,"h":128,"nm":"{name}","ddd":0,"assets":[],"layers":[{{"ddd":0,"ind":1,"ty":4,"nm":"s","sr":1,"ks":{{"o":{{"a":0,"k":100}},"r":{{"a":0,"k":0}},"p":{{"a":0,"k":[64,64,0]}},"a":{{"a":0,"k":[0,0,0]}},"s":{{"a":0,"k":[100,100,100]}}}},"ao":0,"shapes":[{{"ty":"rc","d":1,"s":{{"a":0,"k":[64,64]}},"p":{{"a":0,"k":[0,0]}},"r":{{"a":0,"k":8}}}},{{"ty":"fl","c":{{"a":0,"k":[0.2,0.6,0.9,1]}},"o":{{"a":0,"k":100}}}}],"ip":0,"op":30,"st":0}}]}}"#
    )
}

/// Wraps `raw` into a valid RFC 1952 gzip stream using stored deflate blocks so `.tgs`
/// decompression via `tg_lottie::unpack_gzip` is exercised without adding a `flate2` dep.
fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 32);
    out.extend_from_slice(&[0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xFF]);
    let mut offset = 0;
    loop {
        let remaining = raw.len().saturating_sub(offset);
        let chunk_len = remaining.min(65_535);
        let is_final = offset + chunk_len >= raw.len();
        out.push(if is_final { 0x01 } else { 0x00 });
        let len_u16 = chunk_len as u16;
        out.extend_from_slice(&len_u16.to_le_bytes());
        out.extend_from_slice(&(!len_u16).to_le_bytes());
        out.extend_from_slice(&raw[offset..offset + chunk_len]);
        offset += chunk_len;
        if is_final {
            break;
        }
    }
    let crc = tg_theme::crc32(raw);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out
}

fn make_tl_sticker_doc(id: i64, alt: &str) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 13,
        file_reference: vec![1, 2, 3, (id & 0xFF) as u8],
        date: 1_730_000_000,
        mime_type: "application/x-tgsticker".to_owned(),
        size: 512,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                w: 512,
                h: 512,
            }),
            tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
                mask: false,
                alt: alt.to_owned(),
                stickerset: tl_enums::InputStickerSet::Empty,
                mask_coords: None,
            }),
        ],
    })
}

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default();
        for &(name, data) in entries {
            writer.start_file(name, opts).expect("zip start_file");
            writer.write_all(data).expect("zip write");
        }
        writer.finish().expect("zip finish");
    }
    cursor.into_inner()
}

// =============================================================================
// 1. Dice Sticker Pack (`InputStickerSetDice`) & `messages.getEmojiGameInfo`
// =============================================================================

#[test]
fn dice_pack_fetches_input_sticker_set_dice_and_binds_lottie_json() {
    let (live, fake, _cache_dir) = make_live_session();
    assert_eq!(normalize_dice_emoji("🎲\u{FE0F}"), DICE_EMOJI);
    assert!(is_slot_emoji("🎰\u{FE0F}"));
    assert!(!is_slot_emoji(DICE_EMOJI));

    // Enqueue `messages.getStickerSet` response for `InputStickerSetDice("🎲")`.
    let docs = vec![
        make_tl_sticker_doc(9000, "#"),
        make_tl_sticker_doc(9001, "1"),
        make_tl_sticker_doc(9002, "2"),
        make_tl_sticker_doc(9003, "3"),
        make_tl_sticker_doc(9004, "4"),
        make_tl_sticker_doc(9005, "5"),
        make_tl_sticker_doc(9006, "6"),
    ];
    let packs = vec![
        ("#", 9000i64),
        ("1", 9001),
        ("2", 9002),
        ("3", 9003),
        ("4", 9004),
        ("5", 9005),
        ("6", 9006),
    ]
    .into_iter()
    .map(|(e, id)| {
        tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: e.to_owned(),
            documents: vec![id],
        })
    })
    .collect();

    let sticker_set_resp =
        tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                archived: false,
                official: true,
                masks: false,
                emojis: false,
                text_color: false,
                channel_emoji_status: false,
                creator: false,
                installed_date: None,
                id: 777,
                access_hash: 888,
                title: "Dice".to_owned(),
                short_name: "AnimatedDice".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 7,
                hash: 12345,
            }),
            packs,
            keywords: Vec::new(),
            documents: docs,
        });
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &sticker_set_resp,
    );

    // Enqueue `upload.getFile` for dice face 6 `.tgs` gzipped Lottie JSON.
    let face6_lottie = minimal_lottie_json("dice_face_6_official");
    let face6_tgs = gzip_bytes(face6_lottie.as_bytes());
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: 1_730_000_000,
            bytes: face6_tgs,
        }),
    );

    let mapped = fetch_dice_pack(&live, "🎲\u{FE0F}", Some(6)).expect("fetch_dice_pack");
    assert_eq!(mapped, 7);

    // Verify wire request carried `InputStickerSetDice { emoticon: "🎲" }`.
    let payloads = fake.recorded_payloads(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID);
    assert_eq!(payloads.len(), 1);
    let sent = tl_fns::messages::GetStickerSet::from_bytes(&payloads[0]).expect("decode req");
    assert!(matches!(
        sent.stickerset,
        tl_enums::InputStickerSet::Dice(tl_types::InputStickerSetDice { ref emoticon })
            if emoticon == DICE_EMOJI
    ));

    // Verify dice pack snapshot contains real decompressed Lottie JSON for value 6.
    let pack = snapshot_dice_pack(&live, DICE_EMOJI).expect("dice pack snapshot");
    assert!(pack.is_loaded_from_server());
    let entry6 = pack.get(6).expect("face 6 entry");
    assert_eq!(entry6.lottie_json.as_deref(), Some(face6_lottie.as_str()));
    assert!(!entry6.is_local_fallback);
    tg_lottie::Composition::from_json(entry6.lottie_json.as_deref().expect("json"))
        .expect("dice lottie parses in tg-lottie");

    // Seed a chat with a `MessageMedia::Dice` message and verify `HistoryBackend::load_slice_around`
    // binds the fetched official `lottie_json` and document ID `9006` onto the bubble.
    let peer = DataPeerId::from_user(UserId(2002));
    let ui_peer = from_data_peer(peer);
    {
        let arc = Arc::clone(live.runtime().session());
        live.runtime().tokio_handle().block_on(async move {
            let mut s = arc.write().await;
            let mut u = User::new(UserId(2002), false);
            u.first_name = "Orbit".to_owned();
            u.common.name = "Orbit".to_owned();
            s.upsert_peer(Peer::User(Box::new(u)), PeerUpdateFlags::empty());
            let msg = make_regular_message(
                peer,
                50,
                MessageMedia::Dice(DiceData {
                    value: 6,
                    emoticon: DICE_EMOJI.to_owned(),
                }),
            );
            s.insert_message(msg);
            if let Some(h) = s.history_mut(peer) {
                h.loaded_ranges.add_slice(
                    [DataMsgId(50)],
                    tg_data::MsgRange::new(DataMsgId(50), DataMsgId(50)),
                    Some(1),
                );
            }
        });
    }

    let slice = live.load_slice_around(ui_peer, MsgId(50), 10, 10);
    assert_eq!(slice.messages.len(), 1);
    match &slice.messages[0].media {
        Some(HistoryMedia::Sticker(st)) => {
            assert_eq!(st.sticker_id, 9006);
            assert_eq!(st.emoji, DICE_EMOJI);
            assert_eq!(st.lottie_json, face6_lottie);
        }
        other => panic!("expected HistoryMedia::Sticker for dice, got {other:?}"),
    }

    // Slot machine (`🎰`) local zero fallback + sequential indexing.
    let mut slot_pack = DicePack::new(SLOT_EMOJI);
    slot_pack.try_generate_local_zero();
    for idx in [0, 2, 8, 14, 20] {
        let e = slot_pack.get(idx).expect("slot local idle entry");
        assert!(e.lottie_json.as_deref().is_some_and(|j| !j.is_empty()));
    }

    // `messages.getEmojiGameInfo` + `updateEmojiGameInfo`.
    fake.enqueue_ok(
        tl_fns::messages::GetEmojiGameInfo::CONSTRUCTOR_ID,
        &tl_enums::messages::EmojiGameInfo::EmojiGameDiceInfo(
            tl_types::messages::EmojiGameDiceInfo {
                game_hash: "seed_abc123".to_owned(),
                prev_stake: 500_000_000,
                current_streak: 3,
                params: vec![100, 200, 300, 500, 800, 1500, 5000],
                plays_left: Some(7),
            },
        ),
    );
    let opts = resolve_dice_game_options(&live).expect("resolve_dice_game_options");
    assert!(opts.is_available());
    assert_eq!(opts.seed_hash, "seed_abc123");
    assert_eq!(opts.previous_stake_nano_ton, 500_000_000);
    assert_eq!(opts.milli_rewards, [100, 200, 300, 500, 800, 1500]);
    assert_eq!(opts.jackpot_milli_reward, 5000);
    assert_eq!(opts.current_streak, 3);
    assert_eq!(opts.plays_left, 7);

    let updated = apply_emoji_game_info_update(
        &live,
        &tl_enums::messages::EmojiGameInfo::EmojiGameUnavailable,
    );
    assert!(!updated.is_available());
    assert!(
        !cached_dice_game_options(&live)
            .expect("cached")
            .is_available()
    );
}

// =============================================================================
// 2. `EmojiSetsManager` (cloud download, safe ZIP unpack, & ManageSetsBox UI)
// =============================================================================

#[test]
fn emoji_sets_manager_cloud_download_unpack_and_ui_switch() {
    let (live, fake, _cache_dir) = make_live_session();
    assert_eq!(EMOJI_SETS.len(), 4);
    assert_eq!(EMOJI_SETS[2].name, "Twemoji");
    assert_eq!(EMOJI_SETS[2].post_id, 3224);

    assert!(good_emoji_set_part_name("config.json"));
    assert!(good_emoji_set_part_name("emoji_1.webp"));
    assert!(!good_emoji_set_part_name("../escape.webp"));
    assert!(!good_emoji_set_part_name("nested/config.json"));
    assert!(!good_emoji_set_part_name("script.exe"));

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut mgr = EmojiSetsManager::new(tmp.path().to_path_buf());
    assert_eq!(mgr.current_set_id(), 0);
    assert_eq!(mgr.state_for(0), EmojiSetState::Active);
    assert!(matches!(
        mgr.state_for(2),
        EmojiSetState::Available {
            size_bytes: 6_072_532
        }
    ));

    // Enqueue fake RPCs for `LoadAndSwitchTo(2)` (Twemoji):
    // 1) `contacts.resolveUsername("tdesktop_emoji")`
    fake.enqueue_ok(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 55_001 }),
            chats: vec![tl_enums::Chat::Channel(tl_types::Channel {
                creator: false,
                left: false,
                broadcast: true,
                verified: true,
                megagroup: false,
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
                id: 55_001,
                access_hash: Some(99_001),
                title: "Desktop Emoji".to_owned(),
                username: Some("tdesktop_emoji".to_owned()),
                photo: tl_enums::ChatPhoto::Empty,
                date: 1_730_000_000,
                restriction_reason: None,
                admin_rights: None,
                banned_rights: None,
                default_banned_rights: None,
                participants_count: None,
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
            })],
            users: Vec::new(),
        }),
    );

    // 2) `channels.getMessages(post_id = 3224)` returning the archive document.
    let archive_doc = tl_enums::Document::Document(tl_types::Document {
        id: 88_224,
        access_hash: 123_456,
        file_reference: vec![9, 8, 7],
        date: 1_730_000_000,
        mime_type: "application/zip".to_owned(),
        size: 1024,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl_enums::DocumentAttribute::Filename(
            tl_types::DocumentAttributeFilename {
                file_name: "twemoji.zip".to_owned(),
            },
        )],
    });
    fake.enqueue_ok(
        tl_fns::channels::GetMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 10,
            count: 1,
            offset_id_offset: None,
            messages: vec![tl_enums::Message::Message(tl_types::Message {
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
                id: 3224,
                from_id: None,
                from_boosts_applied: None,
                from_rank: None,
                peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 55_001 }),
                saved_peer_id: None,
                fwd_from: None,
                via_bot_id: None,
                via_business_bot_id: None,
                guestchat_via_from: None,
                reply_to: None,
                date: 1_730_000_000,
                message: String::new(),
                media: Some(tl_enums::MessageMedia::Document(
                    tl_types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: false,
                        document: Some(archive_doc),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                )),
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
            })],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    // 3) `upload.getFile` returning a valid emoji set ZIP archive.
    let valid_zip = build_zip(&[
        ("config.json", br#"{"version":1,"set":"twemoji"}"#),
        ("emoji_0.webp", b"RIFF0000WEBP"),
    ]);
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: 1_730_000_000,
            bytes: valid_zip,
        }),
    );

    let extracted = mgr.load_and_switch_to(&live, 2).expect("load Twemoji set");
    assert_eq!(extracted, 2);
    assert_eq!(mgr.current_set_id(), 2);
    assert_eq!(mgr.state_for(2), EmojiSetState::Active);
    assert_eq!(mgr.state_for(0), EmojiSetState::Ready);
    assert!(mgr.set_data_path(2).join("config.json").is_file());

    // Malicious archive entry is rejected safely.
    let bad_zip = build_zip(&[("../evil.txt", b"nope")]);
    assert!(mgr.finish_download_and_switch(3, &bad_zip).is_err());
    assert!(matches!(mgr.state_for(3), EmojiSetState::Failed(_)));

    // UI switch back to built-in Mac set (id 0) via `egui_kittest`.
    let palette = Palette::for_mode(ThemeMode::Day);
    let mut harness = make_ui_harness(
        Vec2::new(420.0, 400.0),
        |ui, (m, act): &mut (EmojiSetsManager, Option<ManageEmojiSetsAction>)| {
            if let Some(a) = m.show_manage_sets_box(ui, &palette) {
                *act = Some(a);
            }
        },
        (mgr, None),
    );
    harness.run_steps(2);
    // Click first row ("Mac", centered near y=52).
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(Pos2::new(140.0, 52.0)));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: Pos2::new(140.0, 52.0),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: Pos2::new(140.0, 52.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    assert_eq!(harness.state().1, Some(ManageEmojiSetsAction::Switched(0)));
    assert_eq!(harness.state().0.current_set_id(), 0);
}

// =============================================================================
// 3. Interactive Story, Giveaway, GiveawayResults & VideoStream Bubbles
// =============================================================================

#[test]
fn interactive_story_giveaway_and_video_stream_bubbles_convert_and_route_clicks() {
    let mut session = Session::new(UserId(1001));
    let user_id = UserId(3003);
    let mut user = User::new(user_id, false);
    user.first_name = "Nova".to_owned();
    user.common.name = "Nova".to_owned();
    session.upsert_peer(Peer::User(Box::new(user)), PeerUpdateFlags::empty());

    let chan_id = ChannelId(4004);
    let mut ch = Channel::new(chan_id);
    ch.common.name = "Aurora Channel".to_owned();
    session.upsert_peer(Peer::Channel(Box::new(ch)), PeerUpdateFlags::empty());

    let peer = DataPeerId::from_channel(chan_id);

    // 1) Story message -> HistoryMedia::Story (NOT LinkPreviewData).
    let story_msg = make_regular_message(
        peer,
        101,
        MessageMedia::Story(StoryMediaData {
            peer_id: DataPeerId::from_user(user_id),
            story_id: 42,
            via_mention: true,
        }),
    );
    let ui_story = message_to_history_message(&session, &story_msg);
    assert!(ui_story.extra.link_preview.is_none());
    let Some(HistoryMedia::Story(ref st)) = ui_story.media else {
        panic!("expected HistoryMedia::Story, got {:?}", ui_story.media);
    };
    assert_eq!(st.story_id, 42);
    assert!(st.via_mention);
    assert_eq!(st.owner_name, "Nova");

    // 2) Giveaway message -> HistoryMedia::Giveaway (NOT LinkPreviewData).
    let gw_msg = make_regular_message(
        peer,
        102,
        MessageMedia::Giveaway(Box::new(GiveawayData {
            channels: vec![chan_id],
            countries_iso2: vec!["DE".to_owned(), "JP".to_owned()],
            prize_description: Some("Special Badge".to_owned()),
            quantity: 10,
            months: Some(6),
            stars: None,
            until_date: 1_735_000_000,
            only_new_subscribers: true,
            winners_are_visible: true,
        })),
    );
    let ui_gw = message_to_history_message(&session, &gw_msg);
    assert!(ui_gw.extra.link_preview.is_none());
    assert!(matches!(ui_gw.media, Some(HistoryMedia::Giveaway(_))));

    // 3) GiveawayResults message -> HistoryMedia::GiveawayResults (NOT LinkPreviewData).
    let gwr_msg = make_regular_message(
        peer,
        103,
        MessageMedia::GiveawayResults(Box::new(GiveawayResultsData {
            channel_id: chan_id,
            additional_peers_count: 1,
            launch_msg_id: DataMsgId(102),
            winners_count: 10,
            unclaimed_count: 1,
            winners: vec![user_id],
            months: Some(6),
            stars: None,
            prize_description: None,
            until_date: 1_735_000_000,
            only_new_subscribers: false,
            refunded: false,
        })),
    );
    let ui_gwr = message_to_history_message(&session, &gwr_msg);
    assert!(ui_gwr.extra.link_preview.is_none());
    assert!(matches!(
        ui_gwr.media,
        Some(HistoryMedia::GiveawayResults(_))
    ));

    // 4) VideoStream message -> HistoryMedia::VideoStream & ViewerMediaItem(VideoStream).
    let vs_msg = make_regular_message(
        peer,
        104,
        MessageMedia::VideoStream(VideoStreamData {
            call_id: CallId(77_8899),
            access_hash: 55_6677,
            rtmp_stream: true,
        }),
    );
    let ui_vs = message_to_history_message(&session, &vs_msg);
    assert!(matches!(ui_vs.media, Some(HistoryMedia::VideoStream(_))));
    let viewer_vs = message_to_viewer_media_item(&session, &vs_msg).expect("viewer item");
    assert_eq!(viewer_vs.kind, MediaItemKind::VideoStream);

    // Paint and click each interactive bubble via `egui_kittest` and check emitted `MediaInteraction`.
    let click_card = |msg_id: MsgId, media: HistoryMedia| -> Option<MediaInteraction> {
        let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
        let mut harness = make_ui_harness(
            Vec2::new(400.0, 260.0),
            move |ui,
                  state: &mut (
                MediaAssetCache,
                VoicePlaybackState,
                Option<MediaInteraction>,
            )| {
                let rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(300.0, 190.0));
                if let Some(ev) = paint_history_media(
                    ui,
                    msg_id,
                    &media,
                    None,
                    rect,
                    BubbleRounding::default(),
                    false,
                    &colors,
                    &mut state.0,
                    &mut state.1,
                ) {
                    state.2 = Some(ev);
                }
            },
            (MediaAssetCache::new(), VoicePlaybackState::default(), None),
        );
        harness.run_steps(2);
        let center = Pos2::new(160.0, 105.0);
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(center));
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
        harness.state().2.clone()
    };

    assert_eq!(
        click_card(MsgId(101), ui_story.media.clone().expect("story")),
        Some(MediaInteraction::OpenStory {
            peer_id: from_data_peer(DataPeerId::from_user(user_id)),
            story_id: 42,
        })
    );
    assert!(matches!(
        click_card(MsgId(102), ui_gw.media.clone().expect("giveaway")),
        Some(MediaInteraction::OpenGiveawayInfo {
            msg_id: MsgId(102),
            launch_msg_id: None,
            ..
        })
    ));
    assert!(matches!(
        click_card(MsgId(103), ui_gwr.media.clone().expect("giveaway results")),
        Some(MediaInteraction::OpenGiveawayInfo {
            msg_id: MsgId(103),
            launch_msg_id: Some(MsgId(102)),
            ..
        })
    ));
    assert!(matches!(
        click_card(MsgId(104), ui_vs.media.clone().expect("video stream")),
        Some(MediaInteraction::OpenVideoStream {
            msg_id: MsgId(104),
            call_id: 77_8899,
            access_hash: 55_6677,
            rtmp_stream: true,
            ..
        })
    ));

    // Verify `AppStoriesHost::open_story_by_id` opens a peer's specific story directly.
    let mut stories_host = AppStoriesHost::new();
    let first_source_peer = stories_host
        .store()
        .sources(tg_stories::StorySourcesList::NotHidden)
        .first()
        .map(|s| s.id)
        .expect("mock story source");
    assert!(stories_host.open_story_by_id(first_source_peer, None));
    assert!(stories_host.viewer().is_open());
}

// =============================================================================
// 4. Live Video Stream Viewer (`media_view_video_stream`) & Stream RPCs
// =============================================================================

#[test]
fn video_stream_viewer_rpc_donors_frames_and_overlay_controls() {
    let (live, fake, _cache_dir) = make_live_session();

    // Seed a channel peer for `getGroupCallStreamRtmpUrl`.
    let chan_id = ChannelId(6006);
    let data_peer = DataPeerId::from_channel(chan_id);
    {
        let arc = Arc::clone(live.runtime().session());
        live.runtime().tokio_handle().block_on(async move {
            let mut s = arc.write().await;
            let mut ch = Channel::new(chan_id);
            ch.access_hash = 44_3322;
            ch.common.name = "Broadcast Lab".to_owned();
            s.upsert_peer(Peer::Channel(Box::new(ch)), PeerUpdateFlags::empty());
        });
    }

    fake.enqueue_ok(
        tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID,
        &tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
            call: tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
                join_muted: true,
                can_change_join_muted: false,
                join_date_asc: false,
                schedule_start_subscribed: false,
                can_start_video: false,
                record_video_active: false,
                rtmp_stream: true,
                listeners_hidden: false,
                conference: false,
                creator: false,
                messages_enabled: true,
                can_change_messages_enabled: false,
                min: false,
                id: 910_111,
                access_hash: 222_333,
                participants_count: 148,
                title: Some("Launch Stream".to_owned()),
                stream_dc_id: Some(4),
                record_start_date: None,
                schedule_date: None,
                unmuted_video_count: Some(1),
                unmuted_video_limit: 10,
                version: 1,
                invite_link: None,
                send_paid_messages_stars: None,
                default_send_as: None,
            }),
            participants: Vec::new(),
            participants_next_offset: String::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::phone::GetGroupCallStreamChannels::CONSTRUCTOR_ID,
        &tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(
            tl_types::phone::GroupCallStreamChannels {
                channels: vec![tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(
                    tl_types::GroupCallStreamChannel {
                        channel: 1,
                        scale: 0,
                        last_timestamp_ms: 1_730_000_500_000,
                    },
                )],
            },
        ),
    );

    let info = fetch_video_stream_info(&live, 910_111, 222_333).expect("stream info");
    assert_eq!(info.title, "Launch Stream");
    assert_eq!(info.stream_dc_id, 4);
    assert_eq!(info.participants_count, 148);
    assert!(info.rtmp_stream);
    assert_eq!(info.channels.len(), 1);
    assert_eq!(info.channels[0].channel, 1);

    fake.enqueue_ok(
        tl_fns::phone::GetGroupCallStreamRtmpUrl::CONSTRUCTOR_ID,
        &tl_enums::phone::GroupCallStreamRtmpUrl::GroupCallStreamRtmpUrl(
            tl_types::phone::GroupCallStreamRtmpUrl {
                url: "rtmps://dc4-1.rtmp.t.me/s/".to_owned(),
                key: "stream_key_test".to_owned(),
            },
        ),
    );
    let rtmp = fetch_video_stream_rtmp_url(&live, from_data_peer(data_peer), true, false)
        .expect("rtmp url");
    assert_eq!(rtmp.url, "rtmps://dc4-1.rtmp.t.me/s/");
    assert_eq!(rtmp.key, "stream_key_test");

    // Top donors capping (`TopVideoStreamDonors` keeps max 3 non-self donors + `is_my`).
    let donors = vec![
        VideoStreamDonor {
            peer_id: 1,
            name: "A".into(),
            stars: 500,
            is_my: false,
        },
        VideoStreamDonor {
            peer_id: 2,
            name: "Me".into(),
            stars: 400,
            is_my: true,
        },
        VideoStreamDonor {
            peer_id: 3,
            name: "B".into(),
            stars: 300,
            is_my: false,
        },
        VideoStreamDonor {
            peer_id: 4,
            name: "C".into(),
            stars: 200,
            is_my: false,
        },
        VideoStreamDonor {
            peer_id: 5,
            name: "D".into(),
            stars: 100,
            is_my: false,
        },
    ];
    let capped = top_video_stream_donors(&donors);
    assert_eq!(capped.len(), 4);
    assert_eq!(top_donor_places(&donors), vec![1, 2, 3]);

    // Open in MediaOverlayWidget, verify loading -> Live transition on frame push & render.
    let mut overlay = MediaOverlayWidget::new();
    overlay.open_video_stream(
        VideoStreamDescriptor {
            call_id: 910_111,
            access_hash: 222_333,
            peer_id: data_peer.0,
            title: "Launch Stream".to_owned(),
            call_link_slug: String::new(),
            join_msg_id: 104,
            rtmp_stream: true,
            stream_url: Some("rtmps://dc4-1.rtmp.t.me/s/".to_owned()),
            stream_dc_id: 4,
            is_live_story: false,
        },
        1000,
    );
    assert!(overlay.is_open());
    {
        let vs = overlay.video_stream_mut().expect("video stream active");
        assert!(vs.is_loading());
        assert_eq!(*vs.status(), VideoStreamStatus::Connecting);
        vs.update_metadata(None, 148, true);
        vs.set_donors(donors);
        vs.push_comment(VideoStreamComment {
            id: 1,
            sender_peer_id: 1,
            sender_name: "A".to_owned(),
            text: "Great broadcast!".to_owned(),
            paid_stars: 500,
        });
        let rgba = vec![0x33u8; 32 * 18 * 4];
        vs.push_rgba_frame(32, 18, rgba);
        assert!(vs.has_video_frames());
        assert!(!vs.is_loading());
        assert_eq!(*vs.status(), VideoStreamStatus::Live);
        assert_eq!(vs.quality(), VideoStreamQuality::Full);
    }

    let backend = MockMediaBackend::new();
    let mut harness = make_ui_harness(
        Vec2::new(960.0, 640.0),
        move |ui, ov: &mut MediaOverlayWidget| {
            let vp = Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 640.0));
            let _ = ov.show(ui.ctx(), vp, &backend, 2_000);
        },
        overlay,
    );
    harness.run_steps(2);
    assert!(harness.state().is_open());
    assert!(
        harness
            .state()
            .video_stream()
            .expect("vs")
            .has_video_frames()
    );
    let _ = VideoStreamAction::Close;
}

// =============================================================================
// 5. Video-Viewer Sponsored Overlay (`media_view_playback_sponsored`)
// =============================================================================

#[test]
fn playback_sponsored_overlay_timers_pause_countdown_and_events() {
    let ad1 = SponsoredMessageItem {
        random_id: vec![0xAA, 0x01],
        url: "https://t.me/example_sponsor".to_owned(),
        title: "Aurora Tools".to_owned(),
        text: TextWithEntities::simple("Try the new developer kit."),
        button_text: "Open".to_owned(),
        sponsor_info: None,
        additional_info: None,
        recommended: false,
        can_report: true,
        color_index: 1,
        min_display_duration_ms: 4_000,
        max_display_duration_ms: 10_000,
        is_link_internal: true,
        viewed: false,
    };
    let ad2 = SponsoredMessageItem {
        random_id: vec![0xBB, 0x02],
        url: "https://t.me/second_sponsor".to_owned(),
        title: "Nebula Cloud".to_owned(),
        text: TextWithEntities::simple("Fast edge storage."),
        button_text: "View".to_owned(),
        sponsor_info: None,
        additional_info: None,
        recommended: true,
        can_report: true,
        color_index: 2,
        min_display_duration_ms: 2_000,
        max_display_duration_ms: 8_000,
        is_link_internal: true,
        viewed: false,
    };

    let mut sp = PlaybackSponsoredOverlay::new();
    let _ = sp.set_data(
        SponsoredForVideo {
            list: vec![ad1, ad2],
            start_delay_ms: 500,
            between_delay_ms: 2_000,
            state: SponsoredForVideoState::default(),
        },
        0,
    );
    assert_eq!(
        sp.current_state(0).expect("state").left_till_show_ms,
        SPONSORED_START_DELAY_MIN_MS
    );

    assert!(sp.start(1_000).is_empty());
    assert!(sp.update(1_500).is_empty());
    assert!(sp.visible_message().is_none());

    let evs = sp.update(2_000);
    assert_eq!(
        evs,
        vec![PlaybackSponsoredEvent::Viewed {
            random_id: vec![0xAA, 0x01]
        }]
    );
    assert_eq!(
        sp.visible_message().map(|m| m.title.as_str()),
        Some("Aurora Tools")
    );
    assert!(!sp.can_close_at(2_000));
    assert_eq!(sp.seconds_until_close(2_000), 4);

    assert_eq!(
        sp.click_close_button(3_000),
        Some(PlaybackSponsoredEvent::PromotePremium)
    );
    assert!(sp.visible_message().is_some());

    let _ = sp.set_paused(true, 3_000);
    assert_eq!(sp.seconds_until_close(6_000), 3);
    let _ = sp.set_paused(false, 6_000);
    assert!(!sp.can_close_at(8_500));
    assert!(sp.can_close_at(9_000));

    assert_eq!(
        sp.click_close_button(9_000),
        Some(PlaybackSponsoredEvent::Closed {
            random_id: vec![0xAA, 0x01]
        })
    );
    assert!(sp.visible_message().is_none());
    let evs2 = sp.update(11_000);
    assert_eq!(
        evs2,
        vec![PlaybackSponsoredEvent::Viewed {
            random_id: vec![0xBB, 0x02]
        }]
    );
    assert_eq!(
        sp.visible_message().map(|m| m.title.as_str()),
        Some("Nebula Cloud")
    );

    let _ = sp.update(19_000);
    assert!(sp.visible_message().is_none());
}

// =============================================================================
// 6. Theme-File Preview inside `MediaOverlayWidget`
// =============================================================================

#[test]
fn media_overlay_theme_file_preview_parses_and_emits_apply_cancel_share() {
    assert!(is_theme_file(
        "ocean.tdesktop-theme",
        "application/octet-stream"
    ));
    assert!(is_theme_file("midnight.tdesktop-palette", "text/plain"));
    assert!(is_theme_file(
        "custom.bin",
        "application/x-tgtheme-tdesktop"
    ));
    assert!(!is_theme_file("photo.jpg", "image/jpeg"));

    let palette_bytes = b"windowBg: #102030;\nwindowFg: #eef4ff;\nmsgInBg: #182c42;\nmsgOutBg: #204060;\nactiveButtonBg: #3399ff;\n".to_vec();
    let mut overlay = MediaOverlayWidget::new();
    overlay
        .open_theme_preview_bytes(
            MediaItemId(7777),
            "ocean_night.tdesktop-palette",
            palette_bytes.clone(),
            412,
            1000,
        )
        .expect("open theme preview");
    assert!(overlay.is_open());
    let tp = overlay.theme_preview().expect("theme preview active");
    assert_eq!(tp.theme_name, "ocean_night");
    assert_eq!(tp.users_count, 412);
    assert_eq!(
        tp.palette_color("windowBg", egui::Color32::BLACK),
        egui::Color32::from_rgb(0x10, 0x20, 0x30)
    );
    assert_eq!(
        tp.palette_color("msgOutBg", egui::Color32::BLACK),
        egui::Color32::from_rgb(0x20, 0x40, 0x60)
    );

    let backend = MockMediaBackend::new();
    let mut harness = make_ui_harness(
        Vec2::new(960.0, 640.0),
        move |ui, (ov, events): &mut (MediaOverlayWidget, Vec<OverlayEvent>)| {
            let vp = Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 640.0));
            events.extend(ov.show(ui.ctx(), vp, &backend, 1500));
        },
        (overlay, Vec::new()),
    );
    harness.run_steps(2);
    let card_rect = harness
        .state()
        .0
        .theme_preview()
        .expect("theme preview")
        .last_card_rect();
    assert!(card_rect.width() >= 440.0);

    // Click "Apply Theme" button at bottom-right of `card_rect`.
    let apply_pt = Pos2::new(card_rect.right() - 75.0, card_rect.bottom() - 27.0);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(apply_pt));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: apply_pt,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: apply_pt,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();

    assert!(
        harness.state().1.iter().any(|e| matches!(
            e,
            OverlayEvent::ApplyThemePreview {
                item_id: MediaItemId(7777),
                theme_name,
                raw_bytes,
            } if theme_name == "ocean_night" && *raw_bytes == palette_bytes
        )),
        "expected ApplyThemePreview event, got {:?}",
        harness.state().1
    );

    // Clicking outside the preview card closes the overlay (`_themePreviewRect.contains(pos)`).
    let outside = Pos2::new(10.0, 300.0);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(outside));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: outside,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: outside,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    assert!(!harness.state().0.is_open());
    assert!(
        harness
            .state()
            .1
            .iter()
            .any(|e| matches!(e, OverlayEvent::Closed))
    );
    let _ = MediaItem {
        id: MediaItemId(1),
        peer_id: 0,
        msg_id: 0,
        kind: MediaItemKind::Theme,
        width: 1,
        height: 1,
        duration_ms: 0,
        file_size: 0,
        file_name: String::new(),
        mime_type: String::new(),
        sender_name: String::new(),
        date_text: String::new(),
        caption: TextWithEntities::default(),
        rotation_degrees: 0,
        has_alpha: false,
        allows_forward: true,
        allows_delete: false,
        allows_save: true,
        local_path: None,
    };
}

#[test]
fn dice_pack_async_prefetch_and_video_stream_metadata_hydration() {
    use tg_app::live::dice_pack::{populate_video_stream_metadata, prefetch_dice_pack_if_needed};

    let (live, fake, _cache_dir) = make_live_session();
    let docs = vec![
        make_tl_sticker_doc(9100, "#"),
        make_tl_sticker_doc(9105, "5"),
    ];
    let packs = vec![("#", 9100i64), ("5", 9105)]
        .into_iter()
        .map(|(e, id)| {
            tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: e.to_owned(),
                documents: vec![id],
            })
        })
        .collect();
    let set_resp = tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
        set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
            archived: false,
            official: true,
            masks: false,
            emojis: false,
            text_color: false,
            channel_emoji_status: false,
            creator: false,
            installed_date: None,
            id: 778,
            access_hash: 889,
            title: "Dart".to_owned(),
            short_name: "AnimatedDart".to_owned(),
            thumbs: None,
            thumb_dc_id: None,
            thumb_version: None,
            thumb_document_id: None,
            count: 2,
            hash: 55,
        }),
        packs,
        keywords: Vec::new(),
        documents: docs,
    });
    fake.enqueue_ok(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID, &set_resp);
    let dart_lottie = minimal_lottie_json("dart_bullseye");
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: 1_730_000_000,
            bytes: gzip_bytes(dart_lottie.as_bytes()),
        }),
    );

    assert!(prefetch_dice_pack_if_needed(&live, "🎯", 5));
    // Wait briefly for the spawned tokio prefetch task to complete.
    let mut loaded = false;
    for _ in 0..40 {
        if let Some(pack) = snapshot_dice_pack(&live, "🎯")
            && pack.get(5).and_then(|e| e.lottie_json.as_deref()) == Some(dart_lottie.as_str())
        {
            loaded = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert!(
        loaded,
        "async dice prefetch should populate dart face 5 Lottie JSON"
    );
    // Second prefetch call is a no-op once populated.
    assert!(!prefetch_dice_pack_if_needed(&live, "🎯", 5));

    // Hydrate a VideoStreamViewer from `phone.getGroupCall` + `getGroupCallStreamChannels`.
    fake.enqueue_ok(
        tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID,
        &tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
            call: tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
                join_muted: true,
                can_change_join_muted: false,
                join_date_asc: false,
                schedule_start_subscribed: false,
                can_start_video: false,
                record_video_active: false,
                rtmp_stream: true,
                listeners_hidden: false,
                conference: false,
                creator: false,
                messages_enabled: true,
                can_change_messages_enabled: false,
                min: false,
                id: 55_4433,
                access_hash: 99_8877,
                participants_count: 321,
                title: Some("Hydrated Live Stream".to_owned()),
                stream_dc_id: Some(2),
                record_start_date: None,
                schedule_date: None,
                unmuted_video_count: None,
                unmuted_video_limit: 10,
                version: 1,
                invite_link: None,
                send_paid_messages_stars: None,
                default_send_as: None,
            }),
            participants: Vec::new(),
            participants_next_offset: String::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::phone::GetGroupCallStreamChannels::CONSTRUCTOR_ID,
        &tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(
            tl_types::phone::GroupCallStreamChannels {
                channels: Vec::new(),
            },
        ),
    );
    let mut viewer = tg_app::media::VideoStreamViewer::new(
        VideoStreamDescriptor {
            call_id: 55_4433,
            access_hash: 99_8877,
            peer_id: 0,
            title: "Initial".to_owned(),
            call_link_slug: String::new(),
            join_msg_id: 1,
            rtmp_stream: true,
            stream_url: None,
            stream_dc_id: 0,
            is_live_story: true,
        },
        1.0,
    );
    let info = populate_video_stream_metadata(&live, &mut viewer).expect("hydrate metadata");
    assert_eq!(info.participants_count, 321);
    assert_eq!(viewer.descriptor().title, "Hydrated Live Stream");
}

#[test]
fn settings_chat_exposes_emoji_sets_manager_and_switches() {
    use tg_app::settings::chat::{ChatSettingsState, render_chat_settings_section};
    use tg_app::settings::mock::MockSettingsBackend;
    use tg_app::settings::theme_bridge::LiveThemeColors;

    let state = ChatSettingsState {
        emoji_sets_open: true,
        ..Default::default()
    };
    let mut core = tg_core::CoreSettings::default();
    let mut global = tg_core::GlobalConfig::default();
    let mut live_theme = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);
    let mut backend = MockSettingsBackend::new();
    let colors = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);

    let mut harness = make_ui_harness(
        Vec2::new(480.0, 720.0),
        move |ui, st: &mut ChatSettingsState| {
            let _ = render_chat_settings_section(
                ui,
                st,
                &mut core,
                &mut global,
                &mut live_theme,
                &mut backend,
                &colors,
            );
        },
        state,
    );
    harness.run_steps(2);
    assert!(harness.state().emoji_sets_open);
    assert_eq!(harness.state().emoji_sets_manager.current_set_id(), 0);
}
