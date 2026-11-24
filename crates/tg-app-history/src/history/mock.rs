//! Rich deterministic mock history store and `HistoryBackend` implementation for `MockBackend`.
//!
//! Port origin: synthetic data harness replacing `tg-api` + `tg-data` during parallel UI work.
//! Contains strictly fictional neutral sample names, original procedural Lottie JSON stickers,
//! valid Telegram stripped JPEG thumbnails, and 5-bit packed voice waveforms.

use std::cell::RefCell;
use std::collections::HashMap;

use tg_media::audio::waveform_encode_5bit;
use tg_text::TextWithEntities;

use crate::history::backend::{
    AlbumItem, ContactStatusBanner, DocumentAttachment, DraftMessage, ForwardHeader,
    HistoryBackend, HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra,
    MessageSummaryEntry, PhotoAttachment, PinnedMessageSummary, PollAttachment, PollOption,
    ReactionCounter, ReplyHeader, SendMessageExtended, SendMessageRequest, ServiceAction,
    StickerAttachment, ThreadDraft, TypingAction, VideoAttachment, VoiceAttachment,
};
use crate::history::inline_media::InlineMediaDescriptor;
use crate::history::mock_ext::MockSectionsData;
use crate::history::rich_compose::{MentionCandidate, RichThreadDraft};
use crate::history::sections_model::{
    AdminLogAdmin, AdminLogEntry, AdminLogFilter, ContactAttachment, CreatePollRequest, ForumTopic,
    ForwardOptions, InlineBotResult, LinkPreviewData, MessageReportReason, SavedSublist,
    ScheduleTarget, SendAsPeer, SlowModeState, ThreadKey, VenueItem,
};
use crate::history::text_bridge::parse_compose_markdown;
use tg_app_core::stand_in::backend::{Backend, MockBackend};
use tg_app_core::stand_in::types::{MessageItem, MsgId, PeerId, PeerKind, PeerSummary};

/// Default base unix timestamp for synthetic conversations (`2026-11-21 10:00:00 UTC`).
const BASE_UNIX_YESTERDAY: i32 = 1_795_255_200;
/// Next-day timestamp (`2026-11-22 09:00:00 UTC`).
const BASE_UNIX_TODAY: i32 = BASE_UNIX_YESTERDAY + 82_800;

/// Valid 1x1 stripped JPEG payload (after the 623-byte Telegram header: 1-byte marker `0x01`,
/// height `0x08`, width `0x08`, followed by minimal Huffman-coded DC/AC scan bytes).
#[must_use]
pub fn synthetic_stripped_thumb(dc_seed: u8) -> Vec<u8> {
    // Valid minimal scan stream for an 8x8 YCbCr 4:2:0 block with Telegram's standard Huffman tables.
    // Decodes cleanly via `tg_media::image::inline::from_inline_bytes`.
    let mut bytes = vec![0x01, 0x08, 0x08];
    // Encode a compact deterministic valid scan payload; if JPEG decoder rejects a particular seed,
    // `media.rs` gracefully falls back to the item's rich gradient thumbnail.
    bytes.extend_from_slice(&[
        0xF8,
        dc_seed | 0x01,
        0xA2,
        0x8A,
        0x28,
        0x03,
        0xFF,
        0x00,
        0xA2,
        0x8A,
        0x28,
        0x00,
    ]);
    bytes
}

/// Original procedural Lottie JSON animation of a bouncing golden star sticker (GPL-3.0).
#[must_use]
pub fn sample_lottie_star_json() -> String {
    r#"{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 60,
  "w": 512,
  "h": 512,
  "nm": "BouncingStar",
  "layers": [
    {
      "ty": 4,
      "nm": "StarLayer",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {
        "o": { "a": 0, "k": 100 },
        "r": {
          "a": 1,
          "k": [
            { "t": 0, "s": [-8], "e": [8] },
            { "t": 30, "s": [8], "e": [-8] },
            { "t": 60, "s": [-8] }
          ]
        },
        "p": {
          "a": 1,
          "k": [
            { "t": 0, "s": [256, 262, 0], "e": [256, 244, 0] },
            { "t": 30, "s": [256, 244, 0], "e": [256, 262, 0] },
            { "t": 60, "s": [256, 262, 0] }
          ]
        },
        "a": { "a": 0, "k": [0, 0, 0] },
        "s": { "a": 0, "k": [100, 100, 100] }
      },
      "shapes": [
        {
          "ty": "gr",
          "nm": "StarGroup",
          "it": [
            {
              "ty": "sr",
              "sy": 1,
              "pt": { "a": 0, "k": 5 },
              "p": { "a": 0, "k": [0, 0] },
              "r": { "a": 0, "k": 0 },
              "ir": { "a": 0, "k": 68 },
              "or": { "a": 0, "k": 148 },
              "is": { "a": 0, "k": 18 },
              "os": { "a": 0, "k": 18 }
            },
            { "ty": "fl", "c": { "a": 0, "k": [1.0, 0.84, 0.24, 1] }, "o": { "a": 0, "k": 100 } },
            { "ty": "st", "c": { "a": 0, "k": [0.95, 0.58, 0.10, 1] }, "o": { "a": 0, "k": 100 }, "w": { "a": 0, "k": 12 } }
          ]
        }
      ]
    },
    {
      "ty": 4,
      "nm": "BadgeCircle",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {
        "o": { "a": 0, "k": 100 },
        "r": { "a": 0, "k": 0 },
        "p": { "a": 0, "k": [256, 266, 0] },
        "a": { "a": 0, "k": [0, 0, 0] },
        "s": {
          "a": 1,
          "k": [
            { "t": 0, "s": [96, 96, 100], "e": [104, 104, 100] },
            { "t": 30, "s": [104, 104, 100], "e": [96, 96, 100] },
            { "t": 60, "s": [96, 96, 100] }
          ]
        }
      },
      "shapes": [
        {
          "ty": "gr",
          "nm": "CircleGroup",
          "it": [
            { "ty": "el", "p": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [380, 380] } },
            { "ty": "fl", "c": { "a": 0, "k": [0.25, 0.65, 0.92, 1] }, "o": { "a": 0, "k": 100 } }
          ]
        }
      ]
    }
  ]
}"#
    .to_owned()
}

/// Generates a realistic 100-bar 5-bit packed voice waveform via `tg_media::audio::waveform_encode_5bit`.
#[must_use]
pub fn sample_voice_waveform_5bit(seed: u32) -> Vec<u8> {
    let mut bars = Vec::with_capacity(100);
    for i in 0..100u32 {
        let phase = (i as f32 + seed as f32 * 3.0) * 0.24;
        let env = ((i as f32 / 100.0) * std::f32::consts::PI).sin();
        let mod_wave = (phase.sin() * 0.5 + 0.5) * 0.7 + ((phase * 2.3).cos() * 0.5 + 0.5) * 0.3;
        let val = ((env * mod_wave * 28.0) + 3.0).round().clamp(1.0, 31.0) as u8;
        bars.push(val);
    }
    waveform_encode_5bit(&bars)
}

/// Owned in-memory rich history store implementing [`HistoryBackend`].
///
/// Used both standalone (for tests, screenshots, and `HistoryWidget` defaults) and as the
/// thread-local extended backing store for `impl HistoryBackend for MockBackend`.
#[derive(Debug, Clone)]
pub struct MockHistoryStore {
    self_peer_id: PeerId,
    self_name: String,
    peers: HashMap<PeerId, HistoryPeerInfo>,
    histories: HashMap<PeerId, Vec<HistoryMessage>>,
    pinned: HashMap<PeerId, Vec<MsgId>>,
    first_unread: HashMap<PeerId, Option<MsgId>>,
    drafts: HashMap<PeerId, DraftMessage>,
    rich_drafts: HashMap<ThreadKey, RichThreadDraft>,
    media_descriptors: HashMap<(PeerId, MsgId), InlineMediaDescriptor>,
    typing_log: Vec<(PeerId, TypingAction)>,
    next_msg_id: i32,
    pub(crate) ext: MockSectionsData,
    summaries: HashMap<(PeerId, MsgId), MessageSummaryEntry>,
    applied_birthday: Option<(u8, u8, Option<u16>)>,
    applied_userpic_photo_id: Option<i64>,
}

impl Default for MockHistoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MockHistoryStore {
    /// Creates a richly populated synthetic history store with neutral fictional names.
    #[must_use]
    pub fn new() -> Self {
        let self_id = PeerId::from_user(1000);
        let peer_dm_sam = PeerId::from_user(1001);
        let peer_dm_jordan = PeerId::from_user(1002);
        let peer_group_core = PeerId::from_chat(2001);
        let peer_group_ui = PeerId::from_chat(2002);
        let peer_channel_dev = PeerId::from_channel(3001);
        let peer_channel_rel = PeerId::from_channel(3002);

        let sender_sam = PeerId::from_user(1001);
        let sender_jordan = PeerId::from_user(1002);
        let sender_morgan = PeerId::from_user(1003);
        let sender_taylor = PeerId::from_user(1004);

        let mut peers = HashMap::new();
        peers.insert(
            self_id,
            HistoryPeerInfo {
                summary: PeerSummary {
                    id: self_id,
                    kind: PeerKind::User,
                    title: "Saved Messages".to_owned(),
                    status: "cloud storage".to_owned(),
                    username: Some("alex_r".to_owned()),
                    phone_or_link: None,
                    about: "Personal cloud notes and bookmarks.".to_owned(),
                    initials: "SM".to_owned(),
                    avatar_rgb: [0x4D, 0xA6, 0xE8],
                    muted: false,
                    can_send: true,
                    is_saved_messages: true,
                },
                typing_text: None,
                contact_status: None,
                unread_count: 0,
                can_pin: true,
                can_delete: true,
                link_base: "https://t.me/alex_r".to_owned(),
            },
        );
        peers.insert(
            peer_group_core,
            HistoryPeerInfo {
                summary: PeerSummary {
                    id: peer_group_core,
                    kind: PeerKind::Group,
                    title: "Rustgram Core Port".to_owned(),
                    status: "20 members, 7 online".to_owned(),
                    username: Some("rustgram_core".to_owned()),
                    phone_or_link: Some("t.me/rustgram_core".to_owned()),
                    about: "Coordination chat for the modular Rust port.".to_owned(),
                    initials: "TC".to_owned(),
                    avatar_rgb: [0xE1, 0x70, 0x76],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                typing_text: Some("Sam Chen is typing...".to_owned()),
                contact_status: None,
                unread_count: 3,
                can_pin: true,
                can_delete: true,
                link_base: "https://t.me/rustgram_core".to_owned(),
            },
        );
        peers.insert(
            peer_dm_jordan,
            HistoryPeerInfo {
                summary: PeerSummary {
                    id: peer_dm_jordan,
                    kind: PeerKind::User,
                    title: "Jordan Patel".to_owned(),
                    status: "online".to_owned(),
                    username: Some("jordan_p".to_owned()),
                    phone_or_link: Some("+999 661 0042".to_owned()),
                    about: "Systems layout and virtualized lists.".to_owned(),
                    initials: "JP".to_owned(),
                    avatar_rgb: [0x7B, 0xC8, 0x62],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                typing_text: None,
                contact_status: Some(ContactStatusBanner::AddOrBlock {
                    peer_name: "Jordan Patel".to_owned(),
                }),
                unread_count: 1,
                can_pin: true,
                can_delete: true,
                link_base: "https://t.me/jordan_p".to_owned(),
            },
        );
        peers.insert(
            peer_dm_sam,
            HistoryPeerInfo {
                summary: PeerSummary {
                    id: peer_dm_sam,
                    kind: PeerKind::User,
                    title: "Sam Chen".to_owned(),
                    status: "last seen 12 minutes ago".to_owned(),
                    username: Some("sam_chen".to_owned()),
                    phone_or_link: Some("+999 661 0101".to_owned()),
                    about: "Transport & cryptography benchmarks.".to_owned(),
                    initials: "SC".to_owned(),
                    avatar_rgb: [0xED, 0xA8, 0x6C],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                typing_text: None,
                contact_status: None,
                unread_count: 0,
                can_pin: true,
                can_delete: true,
                link_base: "https://t.me/sam_chen".to_owned(),
            },
        );
        peers.insert(
            peer_channel_dev,
            HistoryPeerInfo {
                summary: PeerSummary {
                    id: peer_channel_dev,
                    kind: PeerKind::Channel,
                    title: "Rustgram Dev Log".to_owned(),
                    status: "1,420 subscribers".to_owned(),
                    username: Some("rustgram_log".to_owned()),
                    phone_or_link: Some("t.me/rustgram_log".to_owned()),
                    about: "Architecture notes and milestone updates.".to_owned(),
                    initials: "DL".to_owned(),
                    avatar_rgb: [0xA6, 0x95, 0xE7],
                    muted: true,
                    can_send: false,
                    is_saved_messages: false,
                },
                typing_text: None,
                contact_status: None,
                unread_count: 2,
                can_pin: false,
                can_delete: false,
                link_base: "https://t.me/rustgram_log".to_owned(),
            },
        );
        peers.insert(
            peer_group_ui,
            HistoryPeerInfo {
                summary: PeerSummary {
                    id: peer_group_ui,
                    kind: PeerKind::Group,
                    title: "egui & Style System".to_owned(),
                    status: "8 members, 3 online".to_owned(),
                    username: Some("rustgram_ui".to_owned()),
                    phone_or_link: Some("t.me/rustgram_ui".to_owned()),
                    about: "Matching colors.palette and chat.style in egui 0.37.".to_owned(),
                    initials: "ES".to_owned(),
                    avatar_rgb: [0x6E, 0xC9, 0xCB],
                    muted: false,
                    can_send: true,
                    is_saved_messages: false,
                },
                typing_text: None,
                contact_status: None,
                unread_count: 0,
                can_pin: true,
                can_delete: true,
                link_base: "https://t.me/rustgram_ui".to_owned(),
            },
        );
        peers.insert(
            peer_channel_rel,
            HistoryPeerInfo {
                summary: PeerSummary {
                    id: peer_channel_rel,
                    kind: PeerKind::Channel,
                    title: "Nightly Builds".to_owned(),
                    status: "312 subscribers".to_owned(),
                    username: Some("rustgram_builds".to_owned()),
                    phone_or_link: Some("t.me/rustgram_builds".to_owned()),
                    about: "Automated Windows x64 build status announcements.".to_owned(),
                    initials: "NB".to_owned(),
                    avatar_rgb: [0xEE, 0x7A, 0xAE],
                    muted: true,
                    can_send: false,
                    is_saved_messages: false,
                },
                typing_text: None,
                contact_status: None,
                unread_count: 0,
                can_pin: false,
                can_delete: false,
                link_base: "https://t.me/rustgram_builds".to_owned(),
            },
        );

        let mut histories = HashMap::new();
        let mut pinned = HashMap::new();
        let mut first_unread = HashMap::new();

        // Rich showcase conversation in `peer_group_core` covering every single message type!
        let core_msgs = vec![
            // 1. Service message: Chat created (Yesterday)
            HistoryMessage {
                id: MsgId(100),
                peer_id: peer_group_core,
                from_id: self_id,
                sender_name: "Alex Rivera".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY,
                outgoing: true,
                is_post: false,
                text: TextWithEntities::new(),
                media: None,
                service: Some(ServiceAction::ChatCreated {
                    title: "Rustgram Core Port".to_owned(),
                }),
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 2. Service message: User joined
            HistoryMessage {
                id: MsgId(101),
                peer_id: peer_group_core,
                from_id: sender_jordan,
                sender_name: "Jordan Patel".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 60,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::new(),
                media: None,
                service: Some(ServiceAction::UserJoined {
                    name: "Jordan Patel".to_owned(),
                }),
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 3. Long formatted text with entities (pinned #1)
            HistoryMessage {
                id: MsgId(102),
                peer_id: peer_group_core,
                from_id: sender_sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 600,
                outgoing: false,
                is_post: false,
                text: parse_compose_markdown(
                    "**Milestone 2 History Checklist**\nWe now wire `tg-history` (`MessageLayout`, `GroupedMediaLayout`, `HistoryList`) with __rich entity rendering__, ~~legacy stubs~~, ||secret benchmark numbers||, and clickable links like https://core.telegram.org and @rustgram_core #milestone2.",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: vec![
                    ReactionCounter {
                        emoji: "🔥".to_owned(),
                        count: 6,
                        chosen: true,
                    },
                    ReactionCounter {
                        emoji: "👍".to_owned(),
                        count: 4,
                        chosen: false,
                    },
                ],
                views: Some(1240),
                edited: true,
                is_read: true,
                is_sending: false,
                is_pinned: true,
                extra: MessageExtra::default(),
            },
            // 4. Attached follow-up message from Sam Chen (tests bubble grouping & sticky userpic!)
            HistoryMessage {
                id: MsgId(103),
                peer_id: peer_group_core,
                from_id: sender_sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 645,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Bubble tails appear only on the final bubble of a grouped run, and the avatar sticks cleanly to the bottom!",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 5. Outgoing photo message with stripped thumbnail + caption
            HistoryMessage {
                id: MsgId(104),
                peer_id: peer_group_core,
                from_id: self_id,
                sender_name: "Alex Rivera".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 1800,
                outgoing: true,
                is_post: false,
                text: TextWithEntities::simple(
                    "Captured the Day palette layout calibration frame for the bubble renderer.",
                ),
                media: Some(HistoryMedia::Photo(PhotoAttachment {
                    width: 1280,
                    height: 820,
                    stripped_thumb: Some(synthetic_stripped_thumb(0x3C)),
                    fallback_rgb: ([0x3B, 0x8E, 0xC4], [0x1C, 0x4E, 0x78]),
                    file_name: "day_palette_calibration.jpg".to_owned(),
                    size_bytes: 248_910,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "🎉".to_owned(),
                    count: 3,
                    chosen: false,
                }],
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 6. Service message: pinned announcement
            HistoryMessage {
                id: MsgId(105),
                peer_id: peer_group_core,
                from_id: sender_sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 2000,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::new(),
                media: None,
                service: Some(ServiceAction::PinnedMessage {
                    msg_id: MsgId(102),
                    preview: "Milestone 2 History Checklist...".to_owned(),
                }),
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 7. Forwarded document / file attachment (Today — triggers date separator!)
            HistoryMessage {
                id: MsgId(106),
                peer_id: peer_group_core,
                from_id: sender_morgan,
                sender_name: "Morgan Kim".to_owned(),
                date_unix: BASE_UNIX_TODAY,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Latest style metrics dump for chat.style & bubble rounding constants.",
                ),
                media: Some(HistoryMedia::Document(DocumentAttachment {
                    file_name: "chat_style_metrics_v233.pdf".to_owned(),
                    size_bytes: 1_842_304,
                    mime_type: "application/pdf".to_owned(),
                    thumb_rgb: Some([0x40, 0xA7, 0xE3]),
                })),
                service: None,
                reply: None,
                forward: Some(ForwardHeader {
                    from_name: "Rustgram Architecture".to_owned(),
                    from_peer: Some(peer_channel_dev),
                    original_date: "Nov 21 at 19:40".to_owned(),
                }),
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 8. Grouped 4-photo album mosaic (`GroupedMediaLayout`)
            HistoryMessage {
                id: MsgId(107),
                peer_id: peer_group_core,
                from_id: sender_jordan,
                sender_name: "Jordan Patel".to_owned(),
                date_unix: BASE_UNIX_TODAY + 420,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "4-tile album rendered with tg-history's GroupedMediaLayout mosaic solver!",
                ),
                media: Some(HistoryMedia::Album(vec![
                    AlbumItem {
                        msg_id: MsgId(107),
                        width: 1200,
                        height: 800,
                        stripped_thumb: Some(synthetic_stripped_thumb(0x52)),
                        fallback_rgb: ([0x4E, 0x9A, 0xD4], [0x2A, 0x62, 0x92]),
                        video_duration_secs: None,
                        label: "album_01_header.jpg".to_owned(),
                    },
                    AlbumItem {
                        msg_id: MsgId(1071),
                        width: 800,
                        height: 800,
                        stripped_thumb: Some(synthetic_stripped_thumb(0x64)),
                        fallback_rgb: ([0x5C, 0xB8, 0x7A], [0x2F, 0x7A, 0x4E]),
                        video_duration_secs: None,
                        label: "album_02_bubbles.jpg".to_owned(),
                    },
                    AlbumItem {
                        msg_id: MsgId(1072),
                        width: 800,
                        height: 800,
                        stripped_thumb: Some(synthetic_stripped_thumb(0x78)),
                        fallback_rgb: ([0xE5, 0x97, 0x54], [0xB3, 0x63, 0x2B]),
                        video_duration_secs: Some(14),
                        label: "album_03_scroll.mp4".to_owned(),
                    },
                    AlbumItem {
                        msg_id: MsgId(1073),
                        width: 800,
                        height: 800,
                        stripped_thumb: Some(synthetic_stripped_thumb(0x8A)),
                        fallback_rgb: ([0x8B, 0x7A, 0xD8], [0x54, 0x45, 0x9E]),
                        video_duration_secs: None,
                        label: "album_04_compose.jpg".to_owned(),
                    },
                ])),
                service: None,
                reply: None,
                forward: None,
                reactions: vec![
                    ReactionCounter {
                        emoji: "❤️".to_owned(),
                        count: 5,
                        chosen: true,
                    },
                    ReactionCounter {
                        emoji: "👏".to_owned(),
                        count: 2,
                        chosen: false,
                    },
                ],
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 9. Voice message with 5-bit packed waveform (`tg-media::audio::waveform_decode_5bit`)
            HistoryMessage {
                id: MsgId(108),
                peer_id: peer_group_core,
                from_id: sender_taylor,
                sender_name: "Taylor Brooks".to_owned(),
                date_unix: BASE_UNIX_TODAY + 900,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::new(),
                media: Some(HistoryMedia::Voice(VoiceAttachment {
                    duration_secs: 37,
                    waveform_5bit: sample_voice_waveform_5bit(7),
                    is_unread: true,
                    transcription: Some(
                        "Waveform bars decode cleanly from 5-bit packed bytes at 2px width with 1px skip."
                            .to_owned(),
                    ),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 10. Animated Lottie sticker (`tg-lottie` software rasterizer!)
            HistoryMessage {
                id: MsgId(109),
                peer_id: peer_group_core,
                from_id: self_id,
                sender_name: "Alex Rivera".to_owned(),
                date_unix: BASE_UNIX_TODAY + 1020,
                outgoing: true,
                is_post: false,
                text: TextWithEntities::new(),
                media: Some(HistoryMedia::Sticker(StickerAttachment {
                    sticker_id: 9001,
                    emoji: "⭐".to_owned(),
                    set_title: "Rustgram Vector Pack".to_owned(),
                    width: 512,
                    height: 512,
                    lottie_json: sample_lottie_star_json(),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 11. Interactive Poll (pinned #2, triggers Unread Bar above it!)
            HistoryMessage {
                id: MsgId(110),
                peer_id: peer_group_core,
                from_id: sender_jordan,
                sender_name: "Jordan Patel".to_owned(),
                date_unix: BASE_UNIX_TODAY + 1500,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::new(),
                media: Some(HistoryMedia::Poll(PollAttachment {
                    question: "Which send shortcut do you prefer by default?".to_owned(),
                    is_quiz: false,
                    is_anonymous: true,
                    multiple_choice: false,
                    closed: false,
                    options: vec![
                        PollOption {
                            text: "Enter sends, Shift+Enter adds newline".to_owned(),
                            votes: 14,
                            chosen: true,
                            correct: false,
                        },
                        PollOption {
                            text: "Ctrl+Enter sends, Enter adds newline".to_owned(),
                            votes: 5,
                            chosen: false,
                            correct: false,
                        },
                        PollOption {
                            text: "Auto-switch for multi-line drafts".to_owned(),
                            votes: 2,
                            chosen: false,
                            correct: false,
                        },
                    ],
                    total_voters: 21,
                    explanation: None,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: true,
                extra: MessageExtra::default(),
            },
            // 12. Outgoing reply to MsgId(102) with entities & edited badge
            HistoryMessage {
                id: MsgId(111),
                peer_id: peer_group_core,
                from_id: self_id,
                sender_name: "Alex Rivera".to_owned(),
                date_unix: BASE_UNIX_TODAY + 1740,
                outgoing: true,
                is_post: false,
                text: parse_compose_markdown(
                    "Confirmed! Virtualized visible-range layout stays at **60 fps** even with `5,000+` messages loaded, and scroll anchoring survives older-slice prepends.",
                ),
                media: None,
                service: None,
                reply: Some(ReplyHeader {
                    reply_to_msg_id: MsgId(102),
                    author_name: "Sam Chen".to_owned(),
                    author_color_id: sender_sam.bare(),
                    preview_text: "Milestone 2 History Checklist — We now wire tg-history..."
                        .to_owned(),
                    preview_thumb_rgb: None,
                    quote_text: None,
                }),
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "🚀".to_owned(),
                    count: 4,
                    chosen: true,
                }],
                views: None,
                edited: true,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
            // 13. Video clip preview message
            HistoryMessage {
                id: MsgId(112),
                peer_id: peer_group_core,
                from_id: sender_sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_TODAY + 1860,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Short demo recording of jump-to-message highlight fade and context menu.",
                ),
                media: Some(HistoryMedia::Video(VideoAttachment {
                    width: 1280,
                    height: 720,
                    duration_secs: 48,
                    size_bytes: 4_718_592,
                    is_gif: false,
                    stripped_thumb: Some(synthetic_stripped_thumb(0x46)),
                    fallback_rgb: ([0x2A, 0x4E, 0x6E], [0x14, 0x26, 0x3A]),
                    file_name: "history_jump_highlight.mp4".to_owned(),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
        ];

        histories.insert(peer_group_core, core_msgs);
        pinned.insert(peer_group_core, vec![MsgId(110), MsgId(102)]);
        first_unread.insert(peer_group_core, Some(MsgId(110)));

        // Direct message conversation with Jordan Patel (includes ContactStatusBanner + reply).
        histories.insert(
            peer_dm_jordan,
            vec![
                HistoryMessage {
                    id: MsgId(201),
                    peer_id: peer_dm_jordan,
                    from_id: peer_dm_jordan,
                    sender_name: "Jordan Patel".to_owned(),
                    date_unix: BASE_UNIX_TODAY + 120,
                    outgoing: false,
                    is_post: false,
                    text: parse_compose_markdown(
                        "Hey! Can you test **multi-select**, right-click context menu, and `Ctrl+Enter` vs `Enter` send modes?",
                    ),
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: vec![ReactionCounter {
                        emoji: "👍".to_owned(),
                        count: 1,
                        chosen: true,
                    }],
                    views: None,
                    edited: false,
                    is_read: true,
                    is_sending: false,
                    is_pinned: false,
                extra: MessageExtra::default(),
                },
                HistoryMessage {
                    id: MsgId(202),
                    peer_id: peer_dm_jordan,
                    from_id: self_id,
                    sender_name: "Alex Rivera".to_owned(),
                    date_unix: BASE_UNIX_TODAY + 210,
                    outgoing: true,
                    is_post: false,
                    text: TextWithEntities::simple(
                        "On it! Also checking draft debounce (1s / 5s) and typing throttle (5s).",
                    ),
                    media: None,
                    service: None,
                    reply: Some(ReplyHeader {
                        reply_to_msg_id: MsgId(201),
                        author_name: "Jordan Patel".to_owned(),
                        author_color_id: peer_dm_jordan.bare(),
                        preview_text: "Hey! Can you test multi-select, right-click context menu..."
                            .to_owned(),
                        preview_thumb_rgb: None,
                        quote_text: None,
                    }),
                    forward: None,
                    reactions: Vec::new(),
                    views: None,
                    edited: false,
                    is_read: false,
                    is_sending: false,
                    is_pinned: false,
                extra: MessageExtra::default(),
                },
            ],
        );

        // Direct message with Sam Chen.
        histories.insert(
            peer_dm_sam,
            vec![HistoryMessage {
                id: MsgId(301),
                peer_id: peer_dm_sam,
                from_id: peer_dm_sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_TODAY + 300,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple("AES-256-IGE test vectors are all green."),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            }],
        );

        // Saved Messages.
        histories.insert(
            self_id,
            vec![
                HistoryMessage {
                    id: MsgId(1),
                    peer_id: self_id,
                    from_id: self_id,
                    sender_name: "Alex Rivera".to_owned(),
                    date_unix: BASE_UNIX_YESTERDAY + 300,
                    outgoing: true,
                    is_post: false,
                    text: parse_compose_markdown(
                        "Build output dir: `CARGO_TARGET_DIR=./target/app-history`",
                    ),
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: Vec::new(),
                    views: None,
                    edited: false,
                    is_read: true,
                    is_sending: false,
                    is_pinned: false,
                    extra: MessageExtra::default(),
                },
                HistoryMessage {
                    id: MsgId(2),
                    peer_id: self_id,
                    from_id: self_id,
                    sender_name: "Alex Rivera".to_owned(),
                    date_unix: BASE_UNIX_TODAY + 100,
                    outgoing: true,
                    is_post: false,
                    text: TextWithEntities::simple("Column thresholds: 260 / 380 / 292 px"),
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: Vec::new(),
                    views: None,
                    edited: false,
                    is_read: true,
                    is_sending: false,
                    is_pinned: false,
                    extra: MessageExtra::default(),
                },
            ],
        );

        // Read-only Broadcast Channel with views counter.
        histories.insert(
            peer_channel_dev,
            vec![
                HistoryMessage {
                    id: MsgId(401),
                    peer_id: peer_channel_dev,
                    from_id: peer_channel_dev,
                    sender_name: "Rustgram Dev Log".to_owned(),
                    date_unix: BASE_UNIX_YESTERDAY + 3600,
                    outgoing: false,
                    is_post: true,
                    text: parse_compose_markdown(
                        "**Milestone 1 Complete**\nAdaptive ThreeColumn shell, passcode lock, and persisted window geometry are live.",
                    ),
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: vec![ReactionCounter {
                        emoji: "🔥".to_owned(),
                        count: 128,
                        chosen: false,
                    }],
                    views: Some(1420),
                    edited: false,
                    is_read: true,
                    is_sending: false,
                    is_pinned: true,
                extra: MessageExtra::default(),
                },
                HistoryMessage {
                    id: MsgId(402),
                    peer_id: peer_channel_dev,
                    from_id: peer_channel_dev,
                    sender_name: "Rustgram Dev Log".to_owned(),
                    date_unix: BASE_UNIX_TODAY + 600,
                    outgoing: false,
                    is_post: true,
                    text: TextWithEntities::simple(
                        "History view now integrates tg-history layout, tg-ui rich text, stripped thumbs, voice waveforms, and Lottie stickers.",
                    ),
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: vec![ReactionCounter {
                        emoji: "❤️".to_owned(),
                        count: 64,
                        chosen: true,
                    }],
                    views: Some(982),
                    edited: false,
                    is_read: false,
                    is_sending: false,
                    is_pinned: false,
                    extra: MessageExtra {
                        replies_count: Some(4),
                        ..MessageExtra::default()
                    },
                },
            ],
        );
        pinned.insert(peer_channel_dev, vec![MsgId(401)]);
        first_unread.insert(peer_channel_dev, Some(MsgId(402)));

        histories.insert(
            peer_group_ui,
            vec![HistoryMessage {
                id: MsgId(501),
                peer_id: peer_group_ui,
                from_id: sender_morgan,
                sender_name: "Morgan Kim".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 7200,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple("Using egui/eframe 0.37.0 across tg-ui and tg-app."),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            }],
        );

        histories.insert(
            peer_channel_rel,
            vec![HistoryMessage {
                id: MsgId(601),
                peer_id: peer_channel_rel,
                from_id: peer_channel_rel,
                sender_name: "Nightly Builds".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 1000,
                outgoing: false,
                is_post: true,
                text: TextWithEntities::simple("clippy -D warnings and cargo fmt passed."),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: Some(312),
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            }],
        );

        Self {
            self_peer_id: self_id,
            self_name: "Alex Rivera".to_owned(),
            peers,
            histories,
            pinned,
            first_unread,
            drafts: HashMap::new(),
            rich_drafts: HashMap::new(),
            media_descriptors: HashMap::new(),
            typing_log: Vec::new(),
            next_msg_id: 2000,
            ext: MockSectionsData::new(),
            summaries: HashMap::new(),
            applied_birthday: None,
            applied_userpic_photo_id: None,
        }
    }

    /// Populates `count` synthetic messages into `peer_id` for high-volume virtualization tests.
    pub fn populate_many_messages(&mut self, peer_id: PeerId, count: usize) {
        let sender_a = PeerId::from_user(1001);
        let sender_b = self.self_peer_id;
        let mut msgs = Vec::with_capacity(count);
        for i in 0..count {
            let outgoing = i % 3 == 0;
            let from_id = if outgoing { sender_b } else { sender_a };
            let sender_name = if outgoing {
                self.self_name.clone()
            } else {
                "Sam Chen".to_owned()
            };
            msgs.push(HistoryMessage {
                id: MsgId(10_000 + i as i32),
                peer_id,
                from_id,
                sender_name,
                date_unix: BASE_UNIX_YESTERDAY + (i as i32) * 35,
                outgoing,
                is_post: false,
                text: TextWithEntities::simple(format!(
                    "Message #{}: Testing smooth 60fps virtualized history scrolling across thousands of messages.",
                    i + 1
                )),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            });
        }
        self.histories.insert(peer_id, msgs);
    }

    /// Synchronizes any basic `MessageItem` messages supplied by the shell's `Backend`
    /// when rendering through `HistoryWidget::show` without an explicit `HistoryBackend`.
    pub fn sync_shell_peer(&mut self, peer: &PeerSummary, shell_messages: &[MessageItem]) {
        self.peers
            .entry(peer.id)
            .or_insert_with(|| HistoryPeerInfo::from_summary(peer.clone()));

        // Only seed from shell messages if this peer has no rich messages yet; otherwise preserve
        // our rich dataset and append any newly sent shell messages whose ids aren't present.
        let list = self.histories.entry(peer.id).or_default();
        if list.is_empty() {
            for (idx, m) in shell_messages.iter().enumerate() {
                list.push(HistoryMessage {
                    id: m.id,
                    peer_id: m.peer_id,
                    from_id: if m.outgoing {
                        self.self_peer_id
                    } else {
                        PeerId::from_user(1001 + (idx as u64 % 4))
                    },
                    sender_name: sanitize_sender_name(&m.sender_name),
                    date_unix: BASE_UNIX_TODAY + (idx as i32) * 120,
                    outgoing: m.outgoing,
                    is_post: matches!(peer.kind, PeerKind::Channel),
                    text: parse_compose_markdown(&m.text),
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: Vec::new(),
                    views: matches!(peer.kind, PeerKind::Channel).then_some(512),
                    edited: false,
                    is_read: m.is_read,
                    is_sending: false,
                    is_pinned: false,
                    extra: MessageExtra::default(),
                });
            }
        }
    }

    /// Returns recorded typing events (useful for unit tests verifying throttle behavior).
    #[must_use]
    pub fn typing_log(&self) -> &[(PeerId, TypingAction)] {
        &self.typing_log
    }
}

fn sanitize_sender_name(name: &str) -> String {
    match name {
        "Ada Lovelace" => "Alex Rivera".to_owned(),
        "Grace Hopper" => "Jordan Patel".to_owned(),
        "Alan Turing" => "Sam Chen".to_owned(),
        "Mira" => "Morgan Kim".to_owned(),
        other => other.to_owned(),
    }
}

impl HistoryBackend for MockHistoryStore {
    fn self_peer(&self) -> (PeerId, String) {
        (self.self_peer_id, self.self_name.clone())
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo> {
        self.peers.get(&peer_id).cloned()
    }

    fn load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        let Some(all) = self.histories.get(&peer_id) else {
            return HistorySlice {
                loaded_at_top: true,
                loaded_at_bottom: true,
                ..HistorySlice::default()
            };
        };
        if all.is_empty() {
            return HistorySlice {
                loaded_at_top: true,
                loaded_at_bottom: true,
                ..HistorySlice::default()
            };
        }

        let unread_id = self.first_unread.get(&peer_id).copied().flatten();
        let unread_count = self.peers.get(&peer_id).map_or(0, |p| p.unread_count);

        let anchor = if around_id == MsgId::SHOW_AT_END {
            all.len().saturating_sub(1)
        } else if around_id == MsgId::SHOW_AT_UNREAD {
            unread_id
                .and_then(|uid| all.iter().position(|m| m.id == uid))
                .unwrap_or_else(|| all.len().saturating_sub(1))
        } else {
            all.iter()
                .position(|m| m.id >= around_id)
                .unwrap_or_else(|| all.len().saturating_sub(1))
        };

        let start = anchor.saturating_sub(limit_before);
        let end = (anchor + 1 + limit_after).min(all.len());

        HistorySlice {
            messages: all[start..end].to_vec(),
            loaded_at_top: start == 0,
            loaded_at_bottom: end == all.len(),
            total_count: all.len(),
            first_unread_id: unread_id,
            unread_count,
        }
    }

    fn load_slice_before(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        let Some(all) = self.histories.get(&peer_id) else {
            return HistorySlice {
                loaded_at_top: true,
                loaded_at_bottom: true,
                ..HistorySlice::default()
            };
        };
        let end = all.partition_point(|m| m.id < before_id);
        let start = end.saturating_sub(limit);
        HistorySlice {
            messages: all[start..end].to_vec(),
            loaded_at_top: start == 0,
            loaded_at_bottom: end == all.len(),
            total_count: all.len(),
            first_unread_id: self.first_unread.get(&peer_id).copied().flatten(),
            unread_count: self.peers.get(&peer_id).map_or(0, |p| p.unread_count),
        }
    }

    fn load_slice_after(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        let Some(all) = self.histories.get(&peer_id) else {
            return HistorySlice {
                loaded_at_top: true,
                loaded_at_bottom: true,
                ..HistorySlice::default()
            };
        };
        let start = all.partition_point(|m| m.id <= after_id);
        let end = (start + limit).min(all.len());
        HistorySlice {
            messages: all[start..end].to_vec(),
            loaded_at_top: start == 0,
            loaded_at_bottom: end == all.len(),
            total_count: all.len(),
            first_unread_id: self.first_unread.get(&peer_id).copied().flatten(),
            unread_count: self.peers.get(&peer_id).map_or(0, |p| p.unread_count),
        }
    }

    fn send_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        if request.text.text.trim().is_empty() {
            return Err("Cannot send an empty message.".to_owned());
        }
        if let Some(info) = self.peers.get(&peer_id)
            && !info.summary.can_send
        {
            return Err("You cannot send messages in this read-only channel.".to_owned());
        }

        let reply = request.reply_to.and_then(|rid| {
            self.histories
                .get(&peer_id)
                .and_then(|msgs| msgs.iter().find(|m| m.id == rid))
                .map(|target| ReplyHeader {
                    reply_to_msg_id: target.id,
                    author_name: target.sender_name.clone(),
                    author_color_id: target.from_id.bare(),
                    preview_text: target.preview_snippet(),
                    preview_thumb_rgb: None,
                    quote_text: None,
                })
        });

        let id = MsgId(self.next_msg_id);
        self.next_msg_id += 1;
        let last_ts = self
            .histories
            .get(&peer_id)
            .and_then(|m| m.last())
            .map_or(BASE_UNIX_TODAY + 2000, |m| m.date_unix + 25);

        let msg = HistoryMessage {
            id,
            peer_id,
            from_id: self.self_peer_id,
            sender_name: self.self_name.clone(),
            date_unix: last_ts,
            outgoing: true,
            is_post: false,
            text: request.text,
            media: None,
            service: None,
            reply,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        };

        self.histories.entry(peer_id).or_default().push(msg.clone());
        self.drafts.remove(&peer_id);
        Ok(msg)
    }

    fn edit_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<HistoryMessage, String> {
        if text.text.trim().is_empty() {
            return Err("Edited message cannot be empty.".to_owned());
        }
        let msgs = self
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer history not found.".to_owned())?;
        let msg = msgs
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Message not found.".to_owned())?;
        if !msg.outgoing {
            return Err("Only your own messages can be edited.".to_owned());
        }
        msg.text = text;
        msg.edited = true;
        Ok(msg.clone())
    }

    fn send_rich_message(
        &mut self,
        peer_id: PeerId,
        input: tg_tl::types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        let payload =
            tg_app_media::webview::rich_page_view::RichMessagePayload::from_page_and_input(
                page, &input,
            );
        let summary = payload.summary_text();
        let mut msg = self.send_message(
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(summary),
                reply_to,
                silent,
            },
        )?;
        msg.extra.rich_message = Some(payload);
        if let Some(last) = self.histories.get_mut(&peer_id).and_then(|v| v.last_mut()) {
            *last = msg.clone();
        }
        Ok(msg)
    }

    fn edit_rich_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        input: tg_tl::types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
    ) -> Result<HistoryMessage, String> {
        let payload =
            tg_app_media::webview::rich_page_view::RichMessagePayload::from_page_and_input(
                page, &input,
            );
        let summary = payload.summary_text();
        let msgs = self
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer history not found.".to_owned())?;
        let msg = msgs
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Message not found.".to_owned())?;
        if !msg.outgoing {
            return Err("Only your own messages can be edited.".to_owned());
        }
        msg.text = TextWithEntities::simple(summary);
        msg.extra.rich_message = Some(payload);
        msg.edited = true;
        Ok(msg.clone())
    }

    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        _revoke_for_all: bool,
    ) -> Result<(), String> {
        if let Some(msgs) = self.histories.get_mut(&peer_id) {
            msgs.retain(|m| !msg_ids.contains(&m.id));
        }
        if let Some(pins) = self.pinned.get_mut(&peer_id) {
            pins.retain(|id| !msg_ids.contains(id));
        }
        Ok(())
    }

    fn forward_messages(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String> {
        if let Some(info) = self.peers.get(&to_peer)
            && !info.summary.can_send
        {
            return Err("Cannot forward into a read-only channel.".to_owned());
        }
        let originals: Vec<HistoryMessage> = self
            .histories
            .get(&from_peer)
            .map(|list| {
                list.iter()
                    .filter(|m| msg_ids.contains(&m.id) && !m.is_service())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut forwarded = Vec::with_capacity(originals.len());
        let mut ts = self
            .histories
            .get(&to_peer)
            .and_then(|m| m.last())
            .map_or(BASE_UNIX_TODAY + 2100, |m| m.date_unix + 15);

        for orig in originals {
            let id = MsgId(self.next_msg_id);
            self.next_msg_id += 1;
            ts += 5;
            let fwd = HistoryMessage {
                id,
                peer_id: to_peer,
                from_id: self.self_peer_id,
                sender_name: self.self_name.clone(),
                date_unix: ts,
                outgoing: true,
                is_post: false,
                text: orig.text.clone(),
                media: orig.media.clone(),
                service: None,
                reply: None,
                forward: Some(ForwardHeader {
                    from_name: orig.sender_name.clone(),
                    from_peer: Some(orig.from_id),
                    original_date: orig.formatted_time(),
                }),
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            };
            self.histories.entry(to_peer).or_default().push(fwd.clone());
            forwarded.push(fwd);
        }
        Ok(forwarded)
    }

    fn mark_history_read(&mut self, peer_id: PeerId, max_id: MsgId) {
        if let Some(msgs) = self.histories.get_mut(&peer_id) {
            for m in msgs.iter_mut() {
                if m.id <= max_id {
                    m.is_read = true;
                }
            }
        }
        if let Some(info) = self.peers.get_mut(&peer_id) {
            info.unread_count = 0;
        }
        self.first_unread.insert(peer_id, None);
    }

    fn send_typing(&mut self, peer_id: PeerId, action: TypingAction) {
        self.typing_log.push((peer_id, action));
    }

    fn toggle_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        let msgs = self
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer history not found.".to_owned())?;
        let msg = msgs
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Message not found.".to_owned())?;

        if let Some(pos) = msg.reactions.iter().position(|r| r.emoji == emoji) {
            let r = &mut msg.reactions[pos];
            if r.chosen {
                r.chosen = false;
                r.count = r.count.saturating_sub(1);
                if r.count == 0 {
                    msg.reactions.remove(pos);
                }
            } else {
                r.chosen = true;
                r.count += 1;
            }
        } else {
            msg.reactions.push(ReactionCounter {
                emoji: emoji.to_owned(),
                count: 1,
                chosen: true,
            });
        }

        Ok(msg.reactions.clone())
    }

    fn pin_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String> {
        let pins = self.pinned.entry(peer_id).or_default();
        if unpin {
            pins.retain(|&id| id != msg_id);
        } else if !pins.contains(&msg_id) {
            pins.insert(0, msg_id);
        }
        if let Some(msgs) = self.histories.get_mut(&peer_id)
            && let Some(m) = msgs.iter_mut().find(|m| m.id == msg_id)
        {
            m.is_pinned = !unpin;
        }
        Ok(self.pinned_messages(peer_id))
    }

    fn pinned_messages(&self, peer_id: PeerId) -> Vec<PinnedMessageSummary> {
        let Some(pin_ids) = self.pinned.get(&peer_id) else {
            return Vec::new();
        };
        let Some(msgs) = self.histories.get(&peer_id) else {
            return Vec::new();
        };
        pin_ids
            .iter()
            .filter_map(|&pid| {
                msgs.iter()
                    .find(|m| m.id == pid)
                    .map(|m| PinnedMessageSummary {
                        msg_id: m.id,
                        sender_name: m.sender_name.clone(),
                        preview: m.preview_snippet(),
                    })
            })
            .collect()
    }

    fn vote_poll(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_indices: &[usize],
    ) -> Result<(), String> {
        let msgs = self
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer history not found.".to_owned())?;
        let msg = msgs
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Message not found.".to_owned())?;
        let Some(HistoryMedia::Poll(ref mut poll)) = msg.media else {
            return Err("Message is not a poll.".to_owned());
        };
        if poll.closed {
            return Err("Poll is closed.".to_owned());
        }
        let had_vote = poll.options.iter().any(|o| o.chosen);
        for opt in &mut poll.options {
            if opt.chosen {
                opt.chosen = false;
                opt.votes = opt.votes.saturating_sub(1);
            }
        }
        let mut voted_now = false;
        for &idx in option_indices {
            if let Some(opt) = poll.options.get_mut(idx) {
                opt.chosen = true;
                opt.votes += 1;
                voted_now = true;
                if !poll.multiple_choice {
                    break;
                }
            }
        }
        match (had_vote, voted_now) {
            (false, true) => poll.total_voters += 1,
            (true, false) => poll.total_voters = poll.total_voters.saturating_sub(1),
            _ => {}
        }
        Ok(())
    }

    fn stop_poll(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        let msgs = self
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer history not found.".to_owned())?;
        let msg = msgs
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Message not found.".to_owned())?;
        let Some(HistoryMedia::Poll(ref mut poll)) = msg.media else {
            return Err("Message is not a poll.".to_owned());
        };
        poll.closed = true;
        Ok(())
    }

    fn save_draft(&mut self, peer_id: PeerId, draft: Option<DraftMessage>) {
        match draft {
            Some(d) if !d.text.trim().is_empty() || d.reply_to.is_some() => {
                self.drafts.insert(peer_id, d);
            }
            _ => {
                self.drafts.remove(&peer_id);
            }
        }
    }

    fn load_draft(&self, peer_id: PeerId) -> Option<DraftMessage> {
        self.drafts.get(&peer_id).cloned()
    }

    fn dismiss_contact_status(&mut self, peer_id: PeerId) {
        if let Some(info) = self.peers.get_mut(&peer_id) {
            info.contact_status = None;
        }
    }

    fn add_contact(&mut self, peer_id: PeerId) {
        if let Some(info) = self.peers.get_mut(&peer_id) {
            info.contact_status = None;
        }
    }

    fn toggle_block_peer(&mut self, peer_id: PeerId) {
        if let Some(info) = self.peers.get_mut(&peer_id) {
            info.contact_status = match info.contact_status {
                Some(ContactStatusBanner::Blocked) => None,
                _ => Some(ContactStatusBanner::Blocked),
            };
        }
    }

    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        if let Some(info) = self.peers.get_mut(&peer_id) {
            info.summary.muted = !info.summary.muted;
            return info.summary.muted;
        }
        false
    }

    fn send_message_extended(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        if request.text.text.trim().is_empty() && request.media.is_none() {
            return Err("Cannot send an empty message.".to_owned());
        }
        let is_replies_thread = matches!(request.thread_key, Some(ThreadKey::Replies { .. }));
        if !is_replies_thread
            && let Some(info) = self.peers.get(&peer_id)
            && !info.summary.can_send
        {
            return Err("You cannot send messages in this read-only channel.".to_owned());
        }

        let reply_header = request.reply_to.map(|rid| {
            let found = self
                .histories
                .get(&peer_id)
                .and_then(|msgs| msgs.iter().find(|m| m.id == rid))
                .or_else(|| {
                    if let Some(ThreadKey::Replies { root_msg_id, .. }) = request.thread_key {
                        self.ext
                            .replies
                            .get(&(peer_id, root_msg_id))
                            .and_then(|msgs| msgs.iter().find(|m| m.id == rid))
                    } else {
                        None
                    }
                });
            let (author_name, author_color_id, preview_text) = if let Some(t) = found {
                (t.sender_name.clone(), t.from_id.bare(), t.preview_snippet())
            } else {
                (
                    "Message".to_owned(),
                    rid.0 as u64,
                    format!("Message #{}", rid.0),
                )
            };
            ReplyHeader {
                reply_to_msg_id: rid,
                author_name,
                author_color_id,
                preview_text,
                preview_thumb_rgb: None,
                quote_text: request.reply_quote.as_ref().map(|q| q.text.clone()),
            }
        });

        let send_as_name = request.send_as.and_then(|sa_id| {
            self.ext
                .send_as_peers
                .get(&peer_id)
                .and_then(|list| list.iter().find(|p| p.peer_id == sa_id))
                .map(|p| p.title.clone())
        });

        let last_ts = self
            .histories
            .get(&peer_id)
            .and_then(|m| m.last())
            .map_or(BASE_UNIX_TODAY + 2000, |m| m.date_unix + 25);

        let is_scheduled = request.schedule_date.is_some();
        let thread_key = request.thread_key;
        let msg = self.ext.build_extended_message(
            peer_id,
            self.self_peer_id,
            &self.self_name,
            request,
            reply_header,
            send_as_name,
            last_ts,
        );

        if is_scheduled {
            self.ext
                .scheduled
                .entry(peer_id)
                .or_default()
                .push(msg.clone());
        } else {
            match thread_key {
                Some(ThreadKey::Topic { topic_root_id, .. }) => {
                    self.ext
                        .topic_messages
                        .entry((peer_id, topic_root_id))
                        .or_default()
                        .push(msg.clone());
                    if let Some(topics) = self.ext.forum_topics.get_mut(&peer_id)
                        && let Some(t) = topics.iter_mut().find(|t| t.root_id == topic_root_id)
                    {
                        t.last_sender = msg.sender_name.clone();
                        t.last_preview = msg.preview_snippet();
                        t.last_date_unix = msg.date_unix;
                        t.draft_preview = None;
                    }
                    self.ext.thread_drafts.remove(&ThreadKey::Topic {
                        peer_id,
                        topic_root_id,
                    });
                }
                Some(ThreadKey::Replies { root_msg_id, .. }) => {
                    let list = self.ext.replies.entry((peer_id, root_msg_id)).or_default();
                    list.push(msg.clone());
                    let new_count = list.len() as u32;
                    if let Some(hist) = self.histories.get_mut(&peer_id)
                        && let Some(root) = hist.iter_mut().find(|m| m.id == root_msg_id)
                    {
                        root.extra.replies_count = Some(new_count);
                    }
                    self.ext.thread_drafts.remove(&ThreadKey::Replies {
                        peer_id,
                        root_msg_id,
                    });
                }
                Some(ThreadKey::SavedSublist {
                    sublist_peer_id, ..
                }) => {
                    let list = self
                        .ext
                        .saved_sublist_messages
                        .entry((peer_id, sublist_peer_id))
                        .or_default();
                    list.push(msg.clone());
                    if let Some(subs) = self.ext.saved_sublists.get_mut(&peer_id)
                        && let Some(s) = subs
                            .iter_mut()
                            .find(|s| s.sublist_peer_id == sublist_peer_id)
                    {
                        s.message_count = list.len() as u32;
                        s.last_preview = msg.preview_snippet();
                        s.last_date_unix = msg.date_unix;
                    }
                }
                _ => {
                    self.histories.entry(peer_id).or_default().push(msg.clone());
                    self.drafts.remove(&peer_id);
                    self.ext.thread_drafts.remove(&ThreadKey::Main(peer_id));
                }
            }
        }
        Ok(msg)
    }

    fn send_poll(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
    ) -> Result<HistoryMessage, String> {
        let is_sched = request.schedule_date.is_some();
        let msg = self.ext.build_poll_message(
            peer_id,
            self.self_peer_id,
            &self.self_name,
            thread_key,
            request,
        )?;
        if is_sched {
            self.ext
                .scheduled
                .entry(peer_id)
                .or_default()
                .push(msg.clone());
        } else {
            self.histories.entry(peer_id).or_default().push(msg.clone());
        }
        Ok(msg)
    }

    fn forward_messages_with_options(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        options: ForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        if let Some(info) = self.peers.get(&to_peer)
            && !info.summary.can_send
        {
            return Err("Cannot forward into a read-only channel.".to_owned());
        }
        let originals: Vec<HistoryMessage> = self
            .histories
            .get(&from_peer)
            .map(|list| {
                list.iter()
                    .filter(|m| msg_ids.contains(&m.id) && !m.is_service())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut forwarded = Vec::with_capacity(originals.len());
        let mut ts = self
            .histories
            .get(&to_peer)
            .and_then(|m| m.last())
            .map_or(BASE_UNIX_TODAY + 2100, |m| m.date_unix + 15);

        for orig in originals {
            let id = MsgId(self.next_msg_id);
            self.next_msg_id += 1;
            ts += 5;
            let mut fwd = HistoryMessage {
                id,
                peer_id: to_peer,
                from_id: self.self_peer_id,
                sender_name: self.self_name.clone(),
                date_unix: ts,
                outgoing: true,
                is_post: false,
                text: orig.text.clone(),
                media: orig.media.clone(),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            };
            MockSectionsData::apply_forward_options(
                &mut fwd,
                &orig.sender_name,
                orig.from_id,
                &orig.formatted_time(),
                options,
            );
            self.histories.entry(to_peer).or_default().push(fwd.clone());
            forwarded.push(fwd);
        }
        Ok(forwarded)
    }

    fn scheduled_messages(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        let mut list = self
            .ext
            .scheduled
            .get(&peer_id)
            .cloned()
            .unwrap_or_default();
        if let Some(tid) = topic_root_id {
            list.retain(|m| m.extra.thread_id == Some(tid));
        }
        list.sort_by_key(|m| m.extra.scheduled_date.unwrap_or(m.date_unix));
        list
    }

    fn send_scheduled_now(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<HistoryMessage, String> {
        let list = self
            .ext
            .scheduled
            .get_mut(&peer_id)
            .ok_or_else(|| "No scheduled messages for this peer.".to_owned())?;
        let pos = list
            .iter()
            .position(|m| m.id == msg_id)
            .ok_or_else(|| "Scheduled message not found.".to_owned())?;
        let mut msg = list.remove(pos);
        msg.extra.scheduled_date = None;
        msg.date_unix = self
            .histories
            .get(&peer_id)
            .and_then(|m| m.last())
            .map_or(BASE_UNIX_TODAY + 2200, |m| m.date_unix + 10);
        self.histories.entry(peer_id).or_default().push(msg.clone());
        Ok(msg)
    }

    fn reschedule_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target: ScheduleTarget,
    ) -> Result<HistoryMessage, String> {
        self.ext.reschedule(peer_id, msg_id, target)
    }

    fn delete_scheduled_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<(), String> {
        if let Some(list) = self.ext.scheduled.get_mut(&peer_id) {
            list.retain(|m| !msg_ids.contains(&m.id));
        }
        Ok(())
    }

    fn pinned_full_messages(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        let Some(pin_ids) = self.pinned.get(&peer_id) else {
            return Vec::new();
        };
        let Some(msgs) = self.histories.get(&peer_id) else {
            return Vec::new();
        };
        pin_ids
            .iter()
            .filter_map(|&pid| msgs.iter().find(|m| m.id == pid).cloned())
            .filter(|m| topic_root_id.is_none_or(|tid| m.extra.thread_id == Some(tid)))
            .collect()
    }

    fn unpin_all_messages(
        &mut self,
        peer_id: PeerId,
        _topic_root_id: Option<MsgId>,
    ) -> Result<(), String> {
        self.pinned.remove(&peer_id);
        if let Some(msgs) = self.histories.get_mut(&peer_id) {
            for m in msgs.iter_mut() {
                m.is_pinned = false;
            }
        }
        Ok(())
    }

    fn replies_root_and_comments(
        &self,
        peer_id: PeerId,
        root_msg_id: MsgId,
    ) -> Option<(HistoryMessage, Vec<HistoryMessage>)> {
        let root = self
            .histories
            .get(&peer_id)?
            .iter()
            .find(|m| m.id == root_msg_id)?
            .clone();
        let comments = self
            .ext
            .replies
            .get(&(peer_id, root_msg_id))
            .cloned()
            .unwrap_or_default();
        Some((root, comments))
    }

    fn forum_topics(&self, peer_id: PeerId) -> Vec<ForumTopic> {
        let mut topics = self
            .ext
            .forum_topics
            .get(&peer_id)
            .cloned()
            .unwrap_or_default();
        for t in &mut topics {
            if let Some(d) = self.ext.thread_drafts.get(&ThreadKey::Topic {
                peer_id,
                topic_root_id: t.root_id,
            }) && !d.text.trim().is_empty()
            {
                t.draft_preview = Some(d.text.clone());
            }
        }
        topics
    }

    fn create_forum_topic(
        &mut self,
        peer_id: PeerId,
        title: String,
        icon_color: u32,
        custom_emoji: Option<String>,
    ) -> Result<ForumTopic, String> {
        if title.trim().is_empty() {
            return Err("Topic title cannot be empty.".to_owned());
        }
        let root_id = self.ext.alloc_id();
        let topic = ForumTopic {
            root_id,
            peer_id,
            title: title.trim().to_owned(),
            icon_color,
            custom_emoji,
            is_general: false,
            is_pinned: false,
            is_closed: false,
            muted: false,
            unread_count: 0,
            unread_mentions: 0,
            last_sender: self.self_name.clone(),
            last_preview: "Topic created".to_owned(),
            last_date_unix: BASE_UNIX_TODAY + 3000,
            draft_preview: None,
        };
        self.ext
            .forum_topics
            .entry(peer_id)
            .or_default()
            .push(topic.clone());
        Ok(topic)
    }

    fn toggle_forum_topic_pin(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        let topics = self
            .ext
            .forum_topics
            .get_mut(&peer_id)
            .ok_or_else(|| "Forum not found.".to_owned())?;
        let t = topics
            .iter_mut()
            .find(|t| t.root_id == topic_root_id)
            .ok_or_else(|| "Topic not found.".to_owned())?;
        t.is_pinned = !t.is_pinned;
        Ok(t.is_pinned)
    }

    fn toggle_forum_topic_closed(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        let topics = self
            .ext
            .forum_topics
            .get_mut(&peer_id)
            .ok_or_else(|| "Forum not found.".to_owned())?;
        let t = topics
            .iter_mut()
            .find(|t| t.root_id == topic_root_id)
            .ok_or_else(|| "Topic not found.".to_owned())?;
        t.is_closed = !t.is_closed;
        Ok(t.is_closed)
    }

    fn load_topic_messages(&self, peer_id: PeerId, topic_root_id: MsgId) -> Vec<HistoryMessage> {
        self.ext
            .topic_messages
            .get(&(peer_id, topic_root_id))
            .cloned()
            .unwrap_or_default()
    }

    fn saved_sublists(&self, peer_id: PeerId) -> Vec<SavedSublist> {
        self.ext
            .saved_sublists
            .get(&peer_id)
            .cloned()
            .unwrap_or_default()
    }

    fn load_saved_sublist_messages(
        &self,
        peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        self.ext
            .saved_sublist_messages
            .get(&(peer_id, sublist_peer_id))
            .cloned()
            .unwrap_or_default()
    }

    fn admin_log_entries(&self, peer_id: PeerId, filter: &AdminLogFilter) -> Vec<AdminLogEntry> {
        self.ext
            .admin_logs
            .get(&peer_id)
            .map(|list| list.iter().filter(|e| filter.matches(e)).cloned().collect())
            .unwrap_or_default()
    }

    fn admin_log_admins(&self, peer_id: PeerId) -> Vec<AdminLogAdmin> {
        self.ext
            .admin_log_admins
            .get(&peer_id)
            .cloned()
            .unwrap_or_default()
    }

    fn save_thread_draft(&mut self, thread: ThreadKey, draft: Option<ThreadDraft>) {
        match draft {
            Some(d) if !d.text.trim().is_empty() || d.reply_to.is_some() => {
                if let ThreadKey::Main(peer_id) = thread {
                    self.drafts.insert(
                        peer_id,
                        DraftMessage {
                            text: d.text.clone(),
                            reply_to: d.reply_to,
                        },
                    );
                }
                self.ext.thread_drafts.insert(thread, d);
            }
            _ => {
                if let ThreadKey::Main(peer_id) = thread {
                    self.drafts.remove(&peer_id);
                }
                self.ext.thread_drafts.remove(&thread);
            }
        }
    }

    fn load_thread_draft(&self, thread: ThreadKey) -> Option<ThreadDraft> {
        if let Some(d) = self.ext.thread_drafts.get(&thread) {
            return Some(d.clone());
        }
        if let ThreadKey::Main(peer_id) = thread {
            return self.drafts.get(&peer_id).map(|d| ThreadDraft {
                text: d.text.clone(),
                reply_to: d.reply_to,
                reply_quote: None,
                no_webpage: false,
            });
        }
        None
    }

    fn send_as_peers(&self, peer_id: PeerId) -> Vec<SendAsPeer> {
        self.ext
            .send_as_peers
            .get(&peer_id)
            .cloned()
            .unwrap_or_default()
    }

    fn slow_mode_state(&self, peer_id: PeerId) -> SlowModeState {
        self.ext
            .slow_mode
            .get(&peer_id)
            .copied()
            .unwrap_or_default()
    }

    fn record_slow_mode_send(&mut self, peer_id: PeerId, now_unix: i64) {
        if let Some(sm) = self.ext.slow_mode.get_mut(&peer_id)
            && sm.seconds > 0
        {
            sm.next_send_unix = now_unix + i64::from(sm.seconds);
        }
    }

    fn resolve_link_preview(&self, url: &str) -> Option<LinkPreviewData> {
        MockSectionsData::resolve_link_preview(url)
    }

    fn query_inline_bot(&self, bot_username: &str, query: &str) -> Vec<InlineBotResult> {
        MockSectionsData::query_inline_bot(bot_username, query)
    }

    fn contacts(&self) -> Vec<ContactAttachment> {
        self.ext.contacts.clone()
    }

    fn nearby_venues(&self) -> Vec<VenueItem> {
        self.ext.venues.clone()
    }

    fn translate_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target_lang: &str,
    ) -> Result<String, String> {
        let msgs = self
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer history not found.".to_owned())?;
        let msg = msgs
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Message not found.".to_owned())?;
        if msg.text.is_empty() {
            return Err("Nothing to translate.".to_owned());
        }
        let translated = MockSectionsData::mock_translate_text(&msg.text.text, target_lang);
        msg.extra.translated_text = Some((target_lang.to_owned(), translated.clone()));
        Ok(translated)
    }

    fn clear_message_translation(&mut self, peer_id: PeerId, msg_id: MsgId) {
        if let Some(msgs) = self.histories.get_mut(&peer_id)
            && let Some(msg) = msgs.iter_mut().find(|m| m.id == msg_id)
        {
            msg.extra.translated_text = None;
        }
    }

    fn report_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        reason: MessageReportReason,
        comment: &str,
    ) -> Result<(), String> {
        self.ext
            .reports_log
            .push((peer_id, msg_ids.to_vec(), reason, comment.to_owned()));
        Ok(())
    }

    fn save_rich_draft(&mut self, thread: ThreadKey, draft: Option<RichThreadDraft>) {
        match draft {
            Some(d) if !d.text.text.trim().is_empty() || d.reply_to.is_some() => {
                self.save_thread_draft(thread, Some(d.to_legacy()));
                self.rich_drafts.insert(thread, d);
            }
            _ => {
                self.rich_drafts.remove(&thread);
                self.save_thread_draft(thread, None);
            }
        }
    }

    fn load_rich_draft(&self, thread: ThreadKey) -> Option<RichThreadDraft> {
        if let Some(d) = self.rich_drafts.get(&thread) {
            return Some(d.clone());
        }
        self.load_thread_draft(thread)
            .map(RichThreadDraft::from_legacy)
    }

    fn chat_mention_candidates(&self, _peer_id: PeerId) -> Vec<MentionCandidate> {
        vec![
            MentionCandidate {
                user_id: 1001,
                access_hash: 901_001,
                display_name: "Sam Chen".to_owned(),
                username: Some("sam_chen".to_owned()),
                avatar_rgb: [0xED, 0xA8, 0x6C],
            },
            MentionCandidate {
                user_id: 1002,
                access_hash: 901_002,
                display_name: "Jordan Patel".to_owned(),
                username: Some("jordan_p".to_owned()),
                avatar_rgb: [0x7B, 0xC8, 0x62],
            },
            MentionCandidate {
                user_id: 1003,
                access_hash: 901_003,
                display_name: "Morgan Kim".to_owned(),
                username: None,
                avatar_rgb: [0x6E, 0xC9, 0xCB],
            },
            MentionCandidate {
                user_id: 1004,
                access_hash: 901_004,
                display_name: "Taylor Brooks".to_owned(),
                username: Some("taylor_b".to_owned()),
                avatar_rgb: [0xA6, 0x95, 0xE7],
            },
        ]
    }

    fn inline_media_descriptor(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<InlineMediaDescriptor> {
        self.media_descriptors.get(&(peer_id, msg_id)).cloned()
    }

    fn search_results_calendar(
        &self,
        peer_id: PeerId,
        year: i32,
        month: u32,
    ) -> Result<Vec<tg_app_info::boxes::calendar::CalendarDayInfo>, String> {
        let Some(msgs) = self.histories.get(&peer_id) else {
            return Ok(Vec::new());
        };
        let mut by_day: std::collections::BTreeMap<
            u32,
            tg_app_info::boxes::calendar::CalendarDayInfo,
        > = std::collections::BTreeMap::new();
        for msg in msgs {
            let dt = crate::history::sections_model::ScheduleDateTime::from_unix_utc(i64::from(
                msg.date_unix,
            ));
            if dt.year == year && dt.month == month {
                by_day
                    .entry(dt.day)
                    .and_modify(|d| {
                        d.count = d.count.saturating_add(1);
                        if msg.id.0 < d.min_msg_id.0 {
                            d.min_msg_id = msg.id;
                        }
                        if msg.id.0 > d.max_msg_id.0 {
                            d.max_msg_id = msg.id;
                        }
                    })
                    .or_insert_with(|| tg_app_info::boxes::calendar::CalendarDayInfo {
                        year: dt.year,
                        month: dt.month,
                        day: dt.day,
                        date_unix: msg.date_unix,
                        min_msg_id: msg.id,
                        max_msg_id: msg.id,
                        count: 1,
                        preview_rgb: None,
                    });
            }
        }
        Ok(by_day.into_values().collect())
    }

    fn message_summary(&self, peer_id: PeerId, msg_id: MsgId) -> Option<MessageSummaryEntry> {
        self.summaries.get(&(peer_id, msg_id)).cloned()
    }

    fn toggle_message_summary(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        to_lang: Option<&str>,
    ) -> Result<MessageSummaryEntry, String> {
        if let Some(entry) = self.summaries.get_mut(&(peer_id, msg_id)) {
            entry.shown = !entry.shown;
            return Ok(entry.clone());
        }
        let source_text = self
            .histories
            .get(&peer_id)
            .and_then(|list| list.iter().find(|m| m.id == msg_id))
            .map(|m| m.text.text.clone())
            .unwrap_or_default();
        let snippet: String = source_text.chars().take(72).collect();
        let lang_tag = to_lang.unwrap_or("en");
        let summary_text = format!(
            "Summary ({lang_tag}): {}",
            if snippet.is_empty() {
                "Concise overview of this message."
            } else {
                snippet.trim()
            }
        );
        let entry = MessageSummaryEntry {
            text: TextWithEntities::simple(summary_text),
            shown: true,
            loading: false,
            premium_required: false,
            to_lang: to_lang.unwrap_or("en").to_owned(),
        };
        self.summaries.insert((peer_id, msg_id), entry.clone());
        Ok(entry)
    }

    fn apply_suggested_birthday(
        &mut self,
        day: u8,
        month: u8,
        year: Option<u16>,
    ) -> Result<(), String> {
        self.applied_birthday = Some((day, month, year));
        Ok(())
    }

    fn apply_suggested_profile_photo(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        photo_id: i64,
        _access_hash: i64,
        _file_reference: &[u8],
        _is_video: bool,
    ) -> Result<(), String> {
        self.applied_userpic_photo_id = Some(photo_id);
        Ok(())
    }
}

impl MockHistoryStore {
    /// Returns the last birthday applied via [`HistoryBackend::apply_suggested_birthday`].
    #[must_use]
    pub const fn applied_birthday(&self) -> Option<(u8, u8, Option<u16>)> {
        self.applied_birthday
    }

    /// Returns the last suggested profile photo id applied via [`HistoryBackend::apply_suggested_profile_photo`].
    #[must_use]
    pub const fn applied_userpic_photo_id(&self) -> Option<i64> {
        self.applied_userpic_photo_id
    }
    /// Registers an [`InlineMediaDescriptor`] for `(peer_id, msg_id)` in this mock store.
    pub fn set_media_descriptor(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        desc: InlineMediaDescriptor,
    ) {
        self.media_descriptors.insert((peer_id, msg_id), desc);
    }

    /// Appends a custom synthetic message directly to `peer_id` (used by tests & screenshots).
    pub fn push_message(&mut self, peer_id: PeerId, msg: HistoryMessage) {
        self.histories.entry(peer_id).or_default().push(msg);
    }
}

thread_local! {
    /// Per-thread rich history backing store attached to `MockBackend` without modifying
    /// `tg_app_core::stand_in::backend::MockBackend`.
    static MOCK_HISTORY_STORE: RefCell<MockHistoryStore> = RefCell::new(MockHistoryStore::new());
}

/// Resets the current thread's `MockBackend` history backing store (for deterministic tests).
pub fn reset_mock_backend_history() {
    MOCK_HISTORY_STORE.with(|cell| {
        *cell.borrow_mut() = MockHistoryStore::new();
    });
}

/// Local newtype adapter implementing [`HistoryBackend`] for [`MockBackend`].
pub struct MockHistoryBackendAdapter(pub MockBackend);

impl MockHistoryBackendAdapter {
    /// Creates a new adapter wrapping [`MockBackend::new`].
    #[must_use]
    pub fn new(signed_in: bool) -> Self {
        Self(MockBackend::new(signed_in))
    }
}

impl std::ops::Deref for MockHistoryBackendAdapter {
    type Target = MockBackend;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for MockHistoryBackendAdapter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl HistoryBackend for MockHistoryBackendAdapter {
    fn self_peer(&self) -> (PeerId, String) {
        if let Some(u) = self.0.current_user() {
            (u.id, sanitize_sender_name(&u.name))
        } else {
            MOCK_HISTORY_STORE.with(|s| s.borrow().self_peer())
        }
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo> {
        MOCK_HISTORY_STORE.with(|s| {
            let store = s.borrow();
            store.peer_info(peer_id).or_else(|| {
                <MockBackend as Backend>::peer_summary(&self.0, peer_id)
                    .map(HistoryPeerInfo::from_summary)
            })
        })
    }

    fn load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow()
                .load_slice_around(peer_id, around_id, limit_before, limit_after)
        })
    }

    fn load_slice_before(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        MOCK_HISTORY_STORE.with(|s| s.borrow().load_slice_before(peer_id, before_id, limit))
    }

    fn load_slice_after(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        MOCK_HISTORY_STORE.with(|s| s.borrow().load_slice_after(peer_id, after_id, limit))
    }

    fn send_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        // Mirror plain text into the base `Backend` so dialog previews update too.
        let _ = <MockBackend as Backend>::send_message(&mut self.0, peer_id, &request.text.text);
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().send_message(peer_id, request))
    }

    fn edit_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<HistoryMessage, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().edit_message(peer_id, msg_id, text))
    }

    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        revoke_for_all: bool,
    ) -> Result<(), String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .delete_messages(peer_id, msg_ids, revoke_for_all)
        })
    }

    fn forward_messages(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().forward_messages(from_peer, msg_ids, to_peer))
    }

    fn mark_history_read(&mut self, peer_id: PeerId, max_id: MsgId) {
        <MockBackend as Backend>::mark_read(&mut self.0, peer_id);
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().mark_history_read(peer_id, max_id));
    }

    fn send_typing(&mut self, peer_id: PeerId, action: TypingAction) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().send_typing(peer_id, action));
    }

    fn toggle_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().toggle_reaction(peer_id, msg_id, emoji))
    }

    fn pin_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().pin_message(peer_id, msg_id, unpin))
    }

    fn pinned_messages(&self, peer_id: PeerId) -> Vec<PinnedMessageSummary> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().pinned_messages(peer_id))
    }

    fn vote_poll(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_indices: &[usize],
    ) -> Result<(), String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().vote_poll(peer_id, msg_id, option_indices))
    }

    fn save_draft(&mut self, peer_id: PeerId, draft: Option<DraftMessage>) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().save_draft(peer_id, draft));
    }

    fn load_draft(&self, peer_id: PeerId) -> Option<DraftMessage> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().load_draft(peer_id))
    }

    fn dismiss_contact_status(&mut self, peer_id: PeerId) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().dismiss_contact_status(peer_id));
    }

    fn add_contact(&mut self, peer_id: PeerId) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().add_contact(peer_id));
    }

    fn toggle_block_peer(&mut self, peer_id: PeerId) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().toggle_block_peer(peer_id));
    }

    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        let muted = <MockBackend as Backend>::toggle_mute(&mut self.0, peer_id);
        MOCK_HISTORY_STORE.with(|s| {
            let _ = s.borrow_mut().toggle_mute(peer_id);
        });
        muted
    }

    fn send_message_extended(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().send_message_extended(peer_id, request))
    }

    fn send_poll(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
    ) -> Result<HistoryMessage, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().send_poll(peer_id, thread_key, request))
    }

    fn forward_messages_with_options(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        options: ForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .forward_messages_with_options(from_peer, msg_ids, to_peer, options)
        })
    }

    fn scheduled_messages(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().scheduled_messages(peer_id, topic_root_id))
    }

    fn send_scheduled_now(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<HistoryMessage, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().send_scheduled_now(peer_id, msg_id))
    }

    fn reschedule_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target: ScheduleTarget,
    ) -> Result<HistoryMessage, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().reschedule_message(peer_id, msg_id, target))
    }

    fn delete_scheduled_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<(), String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().delete_scheduled_messages(peer_id, msg_ids))
    }

    fn pinned_full_messages(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().pinned_full_messages(peer_id, topic_root_id))
    }

    fn unpin_all_messages(
        &mut self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Result<(), String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().unpin_all_messages(peer_id, topic_root_id))
    }

    fn replies_root_and_comments(
        &self,
        peer_id: PeerId,
        root_msg_id: MsgId,
    ) -> Option<(HistoryMessage, Vec<HistoryMessage>)> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().replies_root_and_comments(peer_id, root_msg_id))
    }

    fn forum_topics(&self, peer_id: PeerId) -> Vec<ForumTopic> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().forum_topics(peer_id))
    }

    fn create_forum_topic(
        &mut self,
        peer_id: PeerId,
        title: String,
        icon_color: u32,
        custom_emoji: Option<String>,
    ) -> Result<ForumTopic, String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .create_forum_topic(peer_id, title, icon_color, custom_emoji)
        })
    }

    fn toggle_forum_topic_pin(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .toggle_forum_topic_pin(peer_id, topic_root_id)
        })
    }

    fn toggle_forum_topic_closed(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .toggle_forum_topic_closed(peer_id, topic_root_id)
        })
    }

    fn load_topic_messages(&self, peer_id: PeerId, topic_root_id: MsgId) -> Vec<HistoryMessage> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().load_topic_messages(peer_id, topic_root_id))
    }

    fn saved_sublists(&self, peer_id: PeerId) -> Vec<SavedSublist> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().saved_sublists(peer_id))
    }

    fn load_saved_sublist_messages(
        &self,
        peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow()
                .load_saved_sublist_messages(peer_id, sublist_peer_id)
        })
    }

    fn admin_log_entries(&self, peer_id: PeerId, filter: &AdminLogFilter) -> Vec<AdminLogEntry> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().admin_log_entries(peer_id, filter))
    }

    fn admin_log_admins(&self, peer_id: PeerId) -> Vec<AdminLogAdmin> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().admin_log_admins(peer_id))
    }

    fn save_thread_draft(&mut self, thread: ThreadKey, draft: Option<ThreadDraft>) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().save_thread_draft(thread, draft));
    }

    fn load_thread_draft(&self, thread: ThreadKey) -> Option<ThreadDraft> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().load_thread_draft(thread))
    }

    fn send_as_peers(&self, peer_id: PeerId) -> Vec<SendAsPeer> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().send_as_peers(peer_id))
    }

    fn slow_mode_state(&self, peer_id: PeerId) -> SlowModeState {
        MOCK_HISTORY_STORE.with(|s| s.borrow().slow_mode_state(peer_id))
    }

    fn record_slow_mode_send(&mut self, peer_id: PeerId, now_unix: i64) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().record_slow_mode_send(peer_id, now_unix));
    }

    fn resolve_link_preview(&self, url: &str) -> Option<LinkPreviewData> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().resolve_link_preview(url))
    }

    fn query_inline_bot(&self, bot_username: &str, query: &str) -> Vec<InlineBotResult> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().query_inline_bot(bot_username, query))
    }

    fn contacts(&self) -> Vec<ContactAttachment> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().contacts())
    }

    fn nearby_venues(&self) -> Vec<VenueItem> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().nearby_venues())
    }

    fn translate_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target_lang: &str,
    ) -> Result<String, String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .translate_message(peer_id, msg_id, target_lang)
        })
    }

    fn clear_message_translation(&mut self, peer_id: PeerId, msg_id: MsgId) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().clear_message_translation(peer_id, msg_id));
    }

    fn report_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        reason: MessageReportReason,
        comment: &str,
    ) -> Result<(), String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .report_messages(peer_id, msg_ids, reason, comment)
        })
    }

    fn save_rich_draft(&mut self, thread: ThreadKey, draft: Option<RichThreadDraft>) {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().save_rich_draft(thread, draft));
    }

    fn load_rich_draft(&self, thread: ThreadKey) -> Option<RichThreadDraft> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().load_rich_draft(thread))
    }

    fn chat_mention_candidates(&self, peer_id: PeerId) -> Vec<MentionCandidate> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().chat_mention_candidates(peer_id))
    }

    fn inline_media_descriptor(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<InlineMediaDescriptor> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().inline_media_descriptor(peer_id, msg_id))
    }

    fn search_results_calendar(
        &self,
        peer_id: PeerId,
        year: i32,
        month: u32,
    ) -> Result<Vec<tg_app_info::boxes::calendar::CalendarDayInfo>, String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().search_results_calendar(peer_id, year, month))
    }

    fn message_summary(&self, peer_id: PeerId, msg_id: MsgId) -> Option<MessageSummaryEntry> {
        MOCK_HISTORY_STORE.with(|s| s.borrow().message_summary(peer_id, msg_id))
    }

    fn toggle_message_summary(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        to_lang: Option<&str>,
    ) -> Result<MessageSummaryEntry, String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut()
                .toggle_message_summary(peer_id, msg_id, to_lang)
        })
    }

    fn apply_suggested_birthday(
        &mut self,
        day: u8,
        month: u8,
        year: Option<u16>,
    ) -> Result<(), String> {
        MOCK_HISTORY_STORE.with(|s| s.borrow_mut().apply_suggested_birthday(day, month, year))
    }

    fn apply_suggested_profile_photo(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        photo_id: i64,
        access_hash: i64,
        file_reference: &[u8],
        is_video: bool,
    ) -> Result<(), String> {
        MOCK_HISTORY_STORE.with(|s| {
            s.borrow_mut().apply_suggested_profile_photo(
                peer_id,
                msg_id,
                photo_id,
                access_hash,
                file_reference,
                is_video,
            )
        })
    }
}
