//! Extended mock dataset and backend state for history sections (scheduled messages,
//! pinned list, replies/comments threads, forum topics & topic histories, saved messages
//! per-chat sublists, channel admin log / recent actions), compose extras (send-as, slow mode,
//! link previews, inline bots, contacts, venues, per-thread drafts), and message actions
//! (polls, forward options, translation hook, reports).
//!
//! Port origin: synthetic data harness extending `crates/tg-app/src/history/mock.rs`
//! with strictly fictional neutral names and deterministic test data.

use std::collections::HashMap;

use tg_chat::send_options::SCHEDULED_UNTIL_ONLINE_TIMESTAMP;
use tg_text::TextWithEntities;

use crate::history::backend::{
    ForwardHeader, HistoryMedia, HistoryMessage, MessageExtra, PollAttachment, PollOption,
    ReactionCounter, ReplyHeader, SendMessageExtended, ThreadDraft,
};
use crate::history::sections_model::{
    AdminLogAdmin, AdminLogEntry, AdminLogFilterFlags, ContactAttachment, CreatePollRequest,
    FORUM_GENERAL_TOPIC_ID, FORUM_TOPIC_ICON_COLORS, ForumTopic, ForwardOptions, InlineBotResult,
    InlineBotResultKind, LinkPreviewData, MessageReportReason, SavedSublist, ScheduleTarget,
    SendAsPeer, SlowModeState, ThreadKey, VenueItem, validate_create_poll,
};
use crate::history::text_bridge::parse_compose_markdown;
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Base timestamp matching `mock.rs` (`2026-11-22 10:00:00 UTC`).
pub const EXT_BASE_UNIX: i32 = 1_795_341_600;

/// Peer ID of the synthetic Forum supergroup (`PeerId::from_chat(2001)`).
pub const DEMO_FORUM_PEER_ID: PeerId = PeerId::from_chat(2001);
/// Peer ID of the synthetic Channel with a Comments thread (`PeerId::from_channel(3001)`).
pub const DEMO_CHANNEL_PEER_ID: PeerId = PeerId::from_channel(3001);
/// Root post message ID in `DEMO_CHANNEL_PEER_ID` with populated comments (`MsgId(402)`).
pub const DEMO_CHANNEL_POST_ID: MsgId = MsgId(402);
/// Self Saved Messages peer ID (`PeerId::from_user(1000)`).
pub const DEMO_SAVED_PEER_ID: PeerId = PeerId::from_user(1000);

/// Extended state stored inside [`crate::history::mock::MockHistoryStore`].
#[derive(Debug, Clone)]
pub struct MockSectionsData {
    /// Scheduled messages per peer.
    pub scheduled: HashMap<PeerId, Vec<HistoryMessage>>,
    /// Channel post discussion comments keyed by `(channel_peer_id, root_msg_id)`.
    pub replies: HashMap<(PeerId, MsgId), Vec<HistoryMessage>>,
    /// Forum topics per forum peer.
    pub forum_topics: HashMap<PeerId, Vec<ForumTopic>>,
    /// Messages inside each forum topic keyed by `(forum_peer_id, topic_root_id)`.
    pub topic_messages: HashMap<(PeerId, MsgId), Vec<HistoryMessage>>,
    /// Saved messages sublists per saved peer.
    pub saved_sublists: HashMap<PeerId, Vec<SavedSublist>>,
    /// Saved sublist message feeds keyed by `(saved_peer_id, sublist_peer_id)`.
    pub saved_sublist_messages: HashMap<(PeerId, PeerId), Vec<HistoryMessage>>,
    /// Channel admin log entries per peer.
    pub admin_logs: HashMap<PeerId, Vec<AdminLogEntry>>,
    /// Channel admins per peer.
    pub admin_log_admins: HashMap<PeerId, Vec<AdminLogAdmin>>,
    /// Per-thread compose drafts.
    pub thread_drafts: HashMap<ThreadKey, ThreadDraft>,
    /// Available "Send As" peers per chat.
    pub send_as_peers: HashMap<PeerId, Vec<SendAsPeer>>,
    /// Slow mode state per chat.
    pub slow_mode: HashMap<PeerId, SlowModeState>,
    /// Recorded message reports `(peer_id, msg_ids, reason, comment)`.
    pub reports_log: Vec<(PeerId, Vec<MsgId>, MessageReportReason, String)>,
    /// Shareable contacts list.
    pub contacts: Vec<ContactAttachment>,
    /// Nearby placeholder venues.
    pub venues: Vec<VenueItem>,
    /// Next allocated scheduled/topic/comment message id.
    pub next_ext_id: i32,
}

impl Default for MockSectionsData {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSectionsData {
    /// Populates realistic fictional demo datasets for every history section and compose extra.
    #[must_use]
    pub fn new() -> Self {
        let self_id = DEMO_SAVED_PEER_ID;
        let sam = PeerId::from_user(1001);
        let jordan = PeerId::from_user(1002);
        let morgan = PeerId::from_user(1003);
        let taylor = PeerId::from_user(1004);
        let forum_id = DEMO_FORUM_PEER_ID;
        let chan_id = DEMO_CHANNEL_PEER_ID;

        // 1. Scheduled messages
        let mut scheduled: HashMap<PeerId, Vec<HistoryMessage>> = HashMap::new();
        scheduled.insert(
            forum_id,
            vec![
                make_msg(
                    MsgId(9001),
                    forum_id,
                    self_id,
                    "Alex Rivera",
                    EXT_BASE_UNIX + 3600,
                    true,
                    "Release checklist reminder: run `cargo clippy --all-targets -- -D warnings` before tagging.",
                    MessageExtra {
                        scheduled_date: Some(EXT_BASE_UNIX + 3600),
                        ..MessageExtra::default()
                    },
                ),
                make_msg(
                    MsgId(9002),
                    forum_id,
                    self_id,
                    "Alex Rivera",
                    EXT_BASE_UNIX + 86_400,
                    true,
                    "Weekly architecture sync notes will be posted at 09:00 UTC.",
                    MessageExtra {
                        scheduled_date: Some(EXT_BASE_UNIX + 86_400),
                        silent: true,
                        ..MessageExtra::default()
                    },
                ),
            ],
        );
        scheduled.insert(
            jordan,
            vec![make_msg(
                MsgId(9003),
                jordan,
                self_id,
                "Alex Rivera",
                SCHEDULED_UNTIL_ONLINE_TIMESTAMP,
                true,
                "Hey Jordan — let me know when you're online to review the virtualized scroll anchor!",
                MessageExtra {
                    scheduled_date: Some(SCHEDULED_UNTIL_ONLINE_TIMESTAMP),
                    ..MessageExtra::default()
                },
            )],
        );

        // 2. Comments / Replies thread for channel post `MsgId(501)` in `peer_channel_dev (3001)`
        let mut replies: HashMap<(PeerId, MsgId), Vec<HistoryMessage>> = HashMap::new();
        let mut c2 = make_msg(
            MsgId(7002),
            chan_id,
            jordan,
            "Jordan Patel",
            EXT_BASE_UNIX - 2400,
            false,
            "Verified 60fps scrolling with 5,000 messages — layout cache only touches visible rows plus overscan.",
            MessageExtra::default(),
        );
        c2.reply = Some(ReplyHeader {
            reply_to_msg_id: MsgId(7001),
            author_name: "Sam Chen".to_owned(),
            author_color_id: sam.bare(),
            author_accent: None,
            preview_text: "Awesome progress! Does the layout cache stay bounded on resize?"
                .to_owned(),
            preview_thumb_rgb: None,
            preview_thumb: None,
            preview_colored_len: 0,
            quote_text: Some("layout cache stay bounded on resize".to_owned()),
        });
        c2.reactions.push(ReactionCounter {
            emoji: "🔥".to_owned(),
            count: 5,
            chosen: true,
        });

        let mut c1 = make_msg(
            MsgId(7001),
            chan_id,
            sam,
            "Sam Chen",
            EXT_BASE_UNIX - 3200,
            false,
            "Awesome progress! Does the layout cache stay bounded on resize?",
            MessageExtra::default(),
        );
        c1.reactions.push(ReactionCounter {
            emoji: "👍".to_owned(),
            count: 4,
            chosen: false,
        });

        let c3 = make_msg(
            MsgId(7003),
            chan_id,
            morgan,
            "Morgan Kim",
            EXT_BASE_UNIX - 1500,
            false,
            "The new Noto Emoji sprite sheet + tg-lottie animated reactions feel super crisp on Day and Night themes!",
            MessageExtra::default(),
        );
        let c4 = make_msg(
            MsgId(7004),
            chan_id,
            self_id,
            "Alex Rivera",
            EXT_BASE_UNIX - 600,
            true,
            "Thanks everyone! Next up: forum topics, admin log filters, and quiz polls.",
            MessageExtra {
                send_as_name: Some("Rustgram Dev Updates".to_owned()),
                ..MessageExtra::default()
            },
        );
        replies.insert((chan_id, DEMO_CHANNEL_POST_ID), vec![c1, c2, c3, c4]);

        // 3. Forum topics (`data/data_forum_topic.cpp` exact 6 colors + General + Custom emoji)
        let mut forum_topics: HashMap<PeerId, Vec<ForumTopic>> = HashMap::new();
        forum_topics.insert(
            forum_id,
            vec![
                ForumTopic {
                    root_id: FORUM_GENERAL_TOPIC_ID,
                    peer_id: forum_id,
                    title: "General".to_owned(),
                    icon_color: FORUM_TOPIC_ICON_COLORS[0].0,
                    custom_emoji: None,
                    is_general: true,
                    is_pinned: true,
                    is_closed: false,
                    muted: false,
                    unread_count: 3,
                    unread_mentions: 1,
                    last_sender: "Sam Chen".to_owned(),
                    last_preview: "All 35 workspace crates compile cleanly on MSVC 14.51!"
                        .to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 300,
                    draft_preview: None,
                },
                ForumTopic {
                    root_id: MsgId(201),
                    peer_id: forum_id,
                    title: "UI & Theme Architecture".to_owned(),
                    icon_color: FORUM_TOPIC_ICON_COLORS[2].0, // violet 0xCB86DB
                    custom_emoji: Some("🎨".to_owned()),
                    is_general: false,
                    is_pinned: true,
                    is_closed: false,
                    muted: false,
                    unread_count: 7,
                    unread_mentions: 0,
                    last_sender: "Morgan Kim".to_owned(),
                    last_preview: "Updated Day/Night palette tokens and bubble rounding."
                        .to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 900,
                    draft_preview: Some("Let's align the schedule box spacing with...".to_owned()),
                },
                ForumTopic {
                    root_id: MsgId(202),
                    peer_id: forum_id,
                    title: "MTProto & Transport".to_owned(),
                    icon_color: FORUM_TOPIC_ICON_COLORS[0].0, // blue 0x6FB9F0
                    custom_emoji: None,
                    is_general: false,
                    is_pinned: false,
                    is_closed: false,
                    muted: false,
                    unread_count: 2,
                    unread_mentions: 0,
                    last_sender: "Sam Chen".to_owned(),
                    last_preview: "PFS temporary key rotation test vectors passed.".to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 2200,
                    draft_preview: None,
                },
                ForumTopic {
                    root_id: MsgId(203),
                    peer_id: forum_id,
                    title: "Audio & Voice Pipeline".to_owned(),
                    icon_color: FORUM_TOPIC_ICON_COLORS[3].0, // green 0x8EEE98
                    custom_emoji: None,
                    is_general: false,
                    is_pinned: false,
                    is_closed: false,
                    muted: false,
                    unread_count: 0,
                    unread_mentions: 0,
                    last_sender: "Taylor Brooks".to_owned(),
                    last_preview: "48kHz Opus voice capture + 5-bit waveform ready.".to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 5400,
                    draft_preview: None,
                },
                ForumTopic {
                    root_id: MsgId(204),
                    peer_id: forum_id,
                    title: "Bug Triage & Benchmarks".to_owned(),
                    icon_color: FORUM_TOPIC_ICON_COLORS[5].0, // red 0xFB6F5F
                    custom_emoji: None,
                    is_general: false,
                    is_pinned: false,
                    is_closed: false,
                    muted: true,
                    unread_count: 12,
                    unread_mentions: 0,
                    last_sender: "Jordan Patel".to_owned(),
                    last_preview: "Zero-allocation visible slice pass verified at 5k items."
                        .to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 9800,
                    draft_preview: None,
                },
                ForumTopic {
                    root_id: MsgId(205),
                    peer_id: forum_id,
                    title: "Release 0.1 Archive".to_owned(),
                    icon_color: FORUM_TOPIC_ICON_COLORS[1].0, // yellow 0xFFD67E
                    custom_emoji: None,
                    is_general: false,
                    is_pinned: false,
                    is_closed: true,
                    muted: true,
                    unread_count: 0,
                    unread_mentions: 0,
                    last_sender: "Alex Rivera".to_owned(),
                    last_preview: "Milestone 1 sign-off complete. Topic closed.".to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 86_400,
                    draft_preview: None,
                },
            ],
        );

        let mut topic_messages: HashMap<(PeerId, MsgId), Vec<HistoryMessage>> = HashMap::new();
        topic_messages.insert(
            (forum_id, MsgId(201)),
            vec![
                make_msg(
                    MsgId(8101),
                    forum_id,
                    morgan,
                    "Morgan Kim",
                    EXT_BASE_UNIX - 4000,
                    false,
                    "Drafting the new forum topic icons using the exact 6 tdesktop hex colors.",
                    MessageExtra {
                        thread_id: Some(MsgId(201)),
                        ..MessageExtra::default()
                    },
                ),
                make_msg(
                    MsgId(8102),
                    forum_id,
                    self_id,
                    "Alex Rivera",
                    EXT_BASE_UNIX - 1200,
                    true,
                    "Looks super sharp! Let's keep General pinned with the `#` badge.",
                    MessageExtra {
                        thread_id: Some(MsgId(201)),
                        ..MessageExtra::default()
                    },
                ),
            ],
        );
        topic_messages.insert(
            (forum_id, MsgId(202)),
            vec![make_msg(
                MsgId(8201),
                forum_id,
                sam,
                "Sam Chen",
                EXT_BASE_UNIX - 2200,
                false,
                "PFS temporary key rotation test vectors passed.",
                MessageExtra {
                    thread_id: Some(MsgId(202)),
                    ..MessageExtra::default()
                },
            )],
        );

        // 4. Saved Messages per-chat sublists
        let mut saved_sublists: HashMap<PeerId, Vec<SavedSublist>> = HashMap::new();
        saved_sublists.insert(
            self_id,
            vec![
                SavedSublist {
                    sublist_peer_id: self_id,
                    title: "My Notes".to_owned(),
                    initials: "MN".to_owned(),
                    avatar_rgb: [0x4D, 0xA6, 0xE8],
                    is_my_notes: true,
                    is_pinned: true,
                    message_count: 2,
                    last_preview: "Build command: CARGO_TARGET_DIR=./target/...".to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 600,
                },
                SavedSublist {
                    sublist_peer_id: forum_id,
                    title: "Rustgram Core Port".to_owned(),
                    initials: "TC".to_owned(),
                    avatar_rgb: [0xE1, 0x70, 0x76],
                    is_my_notes: false,
                    is_pinned: false,
                    message_count: 3,
                    last_preview: "Milestone 2 History Checklist — tg-history + tg-ui".to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 3600,
                },
                SavedSublist {
                    sublist_peer_id: chan_id,
                    title: "Rustgram Dev Updates".to_owned(),
                    initials: "TD".to_owned(),
                    avatar_rgb: [0x65, 0xA8, 0xE0],
                    is_my_notes: false,
                    is_pinned: false,
                    message_count: 1,
                    last_preview: "API layer 233 schema codegen notes".to_owned(),
                    last_date_unix: EXT_BASE_UNIX - 14_400,
                },
            ],
        );

        let mut saved_sublist_messages: HashMap<(PeerId, PeerId), Vec<HistoryMessage>> =
            HashMap::new();
        saved_sublist_messages.insert(
            (self_id, self_id),
            vec![
                make_msg(
                    MsgId(8501),
                    self_id,
                    self_id,
                    "Alex Rivera",
                    EXT_BASE_UNIX - 7200,
                    true,
                    "Remember to verify Day and Night theme contrast on every modal.",
                    MessageExtra {
                        saved_from_peer: Some(self_id),
                        ..MessageExtra::default()
                    },
                ),
                make_msg(
                    MsgId(8502),
                    self_id,
                    self_id,
                    "Alex Rivera",
                    EXT_BASE_UNIX - 600,
                    true,
                    "Build command: CARGO_TARGET_DIR=./target/app-sections cargo test -p tg-app",
                    MessageExtra {
                        saved_from_peer: Some(self_id),
                        ..MessageExtra::default()
                    },
                ),
            ],
        );
        saved_sublist_messages.insert(
            (self_id, forum_id),
            vec![make_msg(
                MsgId(8510),
                self_id,
                sam,
                "Sam Chen",
                EXT_BASE_UNIX - 3600,
                false,
                "Milestone 2 History Checklist — tg-history + tg-ui + tg-media + tg-lottie.",
                MessageExtra {
                    saved_from_peer: Some(forum_id),
                    ..MessageExtra::default()
                },
            )],
        );

        // 5. Channel Admin Log / Recent Actions
        let admins = vec![
            AdminLogAdmin {
                peer_id: self_id,
                name: "Alex Rivera".to_owned(),
                role: "Owner".to_owned(),
            },
            AdminLogAdmin {
                peer_id: sam,
                name: "Sam Chen".to_owned(),
                role: "Admin".to_owned(),
            },
            AdminLogAdmin {
                peer_id: morgan,
                name: "Morgan Kim".to_owned(),
                role: "Moderator".to_owned(),
            },
        ];
        let log_entries = vec![
            AdminLogEntry {
                id: 5006,
                date_unix: EXT_BASE_UNIX - 420,
                actor_id: self_id,
                actor_name: "Alex Rivera".to_owned(),
                flag: AdminLogFilterFlags::EDIT,
                summary: "Edited message #102 in Rustgram Core Port".to_owned(),
                before_text: Some("Milestone 2 checklist (draft v1)".to_owned()),
                after_text: Some(
                    "Milestone 2 History Checklist — wired tg-history, tg-ui, tg-text, tg-lottie."
                        .to_owned(),
                ),
            },
            AdminLogEntry {
                id: 5005,
                date_unix: EXT_BASE_UNIX - 1200,
                actor_id: sam,
                actor_name: "Sam Chen".to_owned(),
                flag: AdminLogFilterFlags::PINNED,
                summary: "Pinned message #102".to_owned(),
                before_text: None,
                after_text: Some("Milestone 2 History Checklist...".to_owned()),
            },
            AdminLogEntry {
                id: 5004,
                date_unix: EXT_BASE_UNIX - 2600,
                actor_id: morgan,
                actor_name: "Morgan Kim".to_owned(),
                flag: AdminLogFilterFlags::TOPICS,
                summary: "Created forum topic \"UI & Theme Architecture\"".to_owned(),
                before_text: None,
                after_text: Some("Icon color: violet (0xCB86DB)".to_owned()),
            },
            AdminLogEntry {
                id: 5003,
                date_unix: EXT_BASE_UNIX - 5100,
                actor_id: self_id,
                actor_name: "Alex Rivera".to_owned(),
                flag: AdminLogFilterFlags::SETTINGS,
                summary: "Changed Slow Mode interval".to_owned(),
                before_text: Some("Off".to_owned()),
                after_text: Some("30 seconds".to_owned()),
            },
            AdminLogEntry {
                id: 5002,
                date_unix: EXT_BASE_UNIX - 8200,
                actor_id: sam,
                actor_name: "Sam Chen".to_owned(),
                flag: AdminLogFilterFlags::DELETE,
                summary: "Deleted spam message from guest_account".to_owned(),
                before_text: Some("Check out this unrelated promotional link!".to_owned()),
                after_text: None,
            },
            AdminLogEntry {
                id: 5001,
                date_unix: EXT_BASE_UNIX - 14_000,
                actor_id: self_id,
                actor_name: "Alex Rivera".to_owned(),
                flag: AdminLogFilterFlags::PROMOTE,
                summary: "Promoted Morgan Kim to Moderator".to_owned(),
                before_text: Some("Member".to_owned()),
                after_text: Some("Can delete messages, pin messages, manage topics".to_owned()),
            },
        ];
        let mut admin_logs = HashMap::new();
        admin_logs.insert(forum_id, log_entries.clone());
        admin_logs.insert(chan_id, log_entries);
        let mut admin_log_admins = HashMap::new();
        admin_log_admins.insert(forum_id, admins.clone());
        admin_log_admins.insert(chan_id, admins);

        // 6. Send-As peers & Slow Mode
        let mut send_as_peers = HashMap::new();
        let aliases = vec![
            SendAsPeer {
                peer_id: self_id,
                title: "Alex Rivera".to_owned(),
                subtitle: "Personal account".to_owned(),
                is_channel: false,
                avatar_rgb: [0x4D, 0xA6, 0xE8],
            },
            SendAsPeer {
                peer_id: chan_id,
                title: "Rustgram Dev Updates".to_owned(),
                subtitle: "Channel · 4.2K subscribers".to_owned(),
                is_channel: true,
                avatar_rgb: [0x65, 0xA8, 0xE0],
            },
        ];
        send_as_peers.insert(forum_id, aliases.clone());
        send_as_peers.insert(chan_id, aliases);

        let mut slow_mode = HashMap::new();
        slow_mode.insert(
            PeerId::from_chat(2002),
            SlowModeState {
                seconds: 30,
                next_send_unix: 0,
            },
        );

        // 7. Contacts & Venues
        let contacts = vec![
            ContactAttachment {
                user_id: Some(sam),
                first_name: "Sam".to_owned(),
                last_name: "Chen".to_owned(),
                phone_number: "+999 661 0101".to_owned(),
            },
            ContactAttachment {
                user_id: Some(jordan),
                first_name: "Jordan".to_owned(),
                last_name: "Patel".to_owned(),
                phone_number: "+999 661 0042".to_owned(),
            },
            ContactAttachment {
                user_id: Some(morgan),
                first_name: "Morgan".to_owned(),
                last_name: "Kim".to_owned(),
                phone_number: "+999 661 0188".to_owned(),
            },
            ContactAttachment {
                user_id: Some(taylor),
                first_name: "Taylor".to_owned(),
                last_name: "Brooks".to_owned(),
                phone_number: "+999 661 0215".to_owned(),
            },
        ];
        let venues = vec![
            VenueItem {
                id: "v1".to_owned(),
                title: "Harbor Coffee Roasters".to_owned(),
                address: "14 Pier Promenade".to_owned(),
                category: "Cafe".to_owned(),
                lat: 47.3772,
                lon: 8.5421,
            },
            VenueItem {
                id: "v2".to_owned(),
                title: "Central Public Library".to_owned(),
                address: "200 Civic Plaza".to_owned(),
                category: "Library".to_owned(),
                lat: 47.3748,
                lon: 8.5446,
            },
            VenueItem {
                id: "v3".to_owned(),
                title: "Riverside Tech Atrium".to_owned(),
                address: "88 Innovation Way".to_owned(),
                category: "Coworking".to_owned(),
                lat: 47.3794,
                lon: 8.5388,
            },
        ];

        Self {
            scheduled,
            replies,
            forum_topics,
            topic_messages,
            saved_sublists,
            saved_sublist_messages,
            admin_logs,
            admin_log_admins,
            thread_drafts: HashMap::new(),
            send_as_peers,
            slow_mode,
            reports_log: Vec::new(),
            contacts,
            venues,
            next_ext_id: 9500,
        }
    }

    /// Allocates a fresh message id.
    pub fn alloc_id(&mut self) -> MsgId {
        let id = MsgId(self.next_ext_id);
        self.next_ext_id += 1;
        id
    }

    /// Builds a [`PollAttachment`] message from a validated [`CreatePollRequest`].
    pub fn build_poll_message(
        &mut self,
        peer_id: PeerId,
        self_peer_id: PeerId,
        self_name: &str,
        thread_key: Option<ThreadKey>,
        req: CreatePollRequest,
    ) -> Result<HistoryMessage, String> {
        validate_create_poll(&req, crate::history::sections_model::POLL_MAX_OPTIONS)
            .map_err(|e| e.message())?;
        let options = req
            .options
            .into_iter()
            .enumerate()
            .map(|(i, text)| PollOption {
                text,
                votes: 0,
                chosen: false,
                correct: req.is_quiz && req.correct_option == Some(i),
            })
            .collect();
        let poll = PollAttachment {
            question: req.question,
            is_quiz: req.is_quiz,
            is_anonymous: req.is_anonymous,
            multiple_choice: req.multiple_choice,
            closed: false,
            options,
            total_voters: 0,
            explanation: req.explanation,
        };
        let id = self.alloc_id();
        Ok(HistoryMessage {
            id,
            peer_id,
            from_id: self_peer_id,
            sender_name: self_name.to_owned(),
            date_unix: req.schedule_date.unwrap_or(EXT_BASE_UNIX + 60),
            outgoing: true,
            is_post: false,
            text: TextWithEntities::default(),
            media: Some(HistoryMedia::Poll(poll)),
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                thread_id: match thread_key {
                    Some(ThreadKey::Topic { topic_root_id, .. }) => Some(topic_root_id),
                    Some(ThreadKey::Replies { root_msg_id, .. }) => Some(root_msg_id),
                    _ => None,
                },
                saved_from_peer: match thread_key {
                    Some(ThreadKey::SavedSublist {
                        sublist_peer_id, ..
                    }) => Some(sublist_peer_id),
                    _ => None,
                },
                scheduled_date: req.schedule_date,
                silent: req.silent,
                ..MessageExtra::default()
            },
        })
    }

    /// Resolves a synthetic link preview card for any `http(s)` URL.
    #[must_use]
    pub fn resolve_link_preview(url: &str) -> Option<LinkPreviewData> {
        let lower = url.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return None;
        }
        let host = url
            .split("://")
            .nth(1)
            .unwrap_or(url)
            .split('/')
            .next()
            .unwrap_or("example.org");
        let (site_name, title, desc, rgb) = if lower.contains("rust-lang") {
            (
                "Rust Lang",
                "Rust Programming Language",
                "A language empowering everyone to build reliable and efficient software.",
                [0xDE, 0x5D, 0x35],
            )
        } else if lower.contains("github.com") {
            (
                "GitHub",
                "Repository & Pull Requests",
                "Modular Rust workspace and automated CI workflows.",
                [0x24, 0x29, 0x2E],
            )
        } else {
            (
                host,
                "Web Page Preview",
                "Tap to open link or customize preview placement above/below text.",
                [0x40, 0xA7, 0xE3],
            )
        };
        Some(LinkPreviewData {
            url: url.to_owned(),
            site_name: site_name.to_owned(),
            title: title.to_owned(),
            description: desc.to_owned(),
            thumb_rgb: Some(rgb),
            large_media: false,
            above_text: false,
        })
    }

    /// Returns mock inline bot results for `@gif`, `@pic`, `@wiki`, or any `@bot`.
    #[must_use]
    pub fn query_inline_bot(bot_username: &str, query: &str) -> Vec<InlineBotResult> {
        let q = query.trim();
        let topic = if q.is_empty() { "trending" } else { q };
        match bot_username.to_ascii_lowercase().as_str() {
            "gif" => vec![
                InlineBotResult {
                    id: "g1".to_owned(),
                    bot_username: bot_username.to_owned(),
                    title: format!("{topic} celebration"),
                    description: "Animated GIF".to_owned(),
                    send_text: format!("[GIF: {topic} celebration]"),
                    thumb_rgb: [0x3B, 0x7A, 0xA8],
                    kind: InlineBotResultKind::Gif,
                },
                InlineBotResult {
                    id: "g2".to_owned(),
                    bot_username: bot_username.to_owned(),
                    title: format!("{topic} thumbs up"),
                    description: "Animated GIF".to_owned(),
                    send_text: format!("[GIF: {topic} thumbs up]"),
                    thumb_rgb: [0x58, 0x9C, 0x62],
                    kind: InlineBotResultKind::Gif,
                },
                InlineBotResult {
                    id: "g3".to_owned(),
                    bot_username: bot_username.to_owned(),
                    title: format!("{topic} applause"),
                    description: "Animated GIF".to_owned(),
                    send_text: format!("[GIF: {topic} applause]"),
                    thumb_rgb: [0x92, 0x5E, 0xB8],
                    kind: InlineBotResultKind::Gif,
                },
            ],
            "pic" => vec![
                InlineBotResult {
                    id: "p1".to_owned(),
                    bot_username: bot_username.to_owned(),
                    title: format!("{topic} landscape"),
                    description: "Photo result".to_owned(),
                    send_text: format!("[Photo: {topic} landscape]"),
                    thumb_rgb: [0x4A, 0x8F, 0xC4],
                    kind: InlineBotResultKind::Photo,
                },
                InlineBotResult {
                    id: "p2".to_owned(),
                    bot_username: bot_username.to_owned(),
                    title: format!("{topic} minimal"),
                    description: "Photo result".to_owned(),
                    send_text: format!("[Photo: {topic} minimal]"),
                    thumb_rgb: [0xC9, 0x7D, 0x4E],
                    kind: InlineBotResultKind::Photo,
                },
            ],
            _ => vec![
                InlineBotResult {
                    id: "a1".to_owned(),
                    bot_username: bot_username.to_owned(),
                    title: format!("Summary: {topic}"),
                    description: format!("Inline article via @{bot_username}"),
                    send_text: format!("{topic} — overview via @{bot_username}"),
                    thumb_rgb: [0x40, 0xA7, 0xE3],
                    kind: InlineBotResultKind::Article,
                },
                InlineBotResult {
                    id: "a2".to_owned(),
                    bot_username: bot_username.to_owned(),
                    title: format!("Reference links for {topic}"),
                    description: "Quick inline card".to_owned(),
                    send_text: format!("Reference notes for {topic} (via @{bot_username})"),
                    thumb_rgb: [0x55, 0xB3, 0x52],
                    kind: InlineBotResultKind::Article,
                },
            ],
        }
    }

    /// Applies [`ForwardOptions`] (`drop_author`, `drop_media_captions`) to a forwarded message.
    pub fn apply_forward_options(
        msg: &mut HistoryMessage,
        orig_sender_name: &str,
        orig_from_id: PeerId,
        orig_time: &str,
        options: ForwardOptions,
    ) {
        if options.drop_author {
            msg.forward = None;
        } else {
            msg.forward = Some(ForwardHeader {
                from_name: orig_sender_name.to_owned(),
                from_peer: Some(orig_from_id),
                original_date: orig_time.to_owned(),
            });
        }
        if options.drop_media_captions && msg.media.is_some() {
            msg.text = TextWithEntities::default();
        }
    }

    /// Generates a deterministic mock translation string for UI previewing.
    #[must_use]
    pub fn mock_translate_text(original: &str, target_lang: &str) -> String {
        let lang = target_lang.to_ascii_uppercase();
        format!("[{lang} translation] {original}")
    }

    /// Applies a reschedule target to a scheduled message.
    pub fn reschedule(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target: ScheduleTarget,
    ) -> Result<HistoryMessage, String> {
        let list = self
            .scheduled
            .get_mut(&peer_id)
            .ok_or_else(|| "No scheduled messages for this peer.".to_owned())?;
        let msg = list
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Scheduled message not found.".to_owned())?;
        let ts = target.to_time_id();
        msg.date_unix = ts;
        msg.extra.scheduled_date = Some(ts);
        Ok(msg.clone())
    }

    /// Builds an outgoing message from [`SendMessageExtended`] for routing to scheduled / topic /
    /// comments / saved-sublist / main history.
    #[allow(clippy::too_many_arguments)]
    pub fn build_extended_message(
        &mut self,
        peer_id: PeerId,
        self_peer_id: PeerId,
        self_name: &str,
        req: SendMessageExtended,
        reply_header: Option<ReplyHeader>,
        send_as_name: Option<String>,
        date_unix: i32,
    ) -> HistoryMessage {
        let id = self.alloc_id();
        let thread_id = match req.thread_key {
            Some(ThreadKey::Topic { topic_root_id, .. }) => Some(topic_root_id),
            Some(ThreadKey::Replies { root_msg_id, .. }) => Some(root_msg_id),
            _ => None,
        };
        let saved_from_peer = match req.thread_key {
            Some(ThreadKey::SavedSublist {
                sublist_peer_id, ..
            }) => Some(sublist_peer_id),
            _ => None,
        };
        let link_preview = if req.no_webpage {
            None
        } else {
            req.link_preview
        };
        HistoryMessage {
            id,
            peer_id,
            from_id: req.send_as.unwrap_or(self_peer_id),
            sender_name: send_as_name.clone().unwrap_or_else(|| self_name.to_owned()),
            date_unix: req.schedule_date.unwrap_or(date_unix),
            outgoing: true,
            is_post: false,
            text: req.text,
            media: req.media,
            service: None,
            reply: reply_header,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                thread_id,
                saved_from_peer,
                scheduled_date: req.schedule_date,
                silent: req.silent,
                effect_id: req.effect_id,
                via_bot: req.via_bot,
                send_as_name,
                translated_text: None,
                link_preview,
                replies_count: None,
                reply_markup: None,
                rich_message: None,
                ..MessageExtra::default()
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn make_msg(
    id: MsgId,
    peer_id: PeerId,
    from_id: PeerId,
    sender_name: &str,
    date_unix: i32,
    outgoing: bool,
    body: &str,
    extra: MessageExtra,
) -> HistoryMessage {
    HistoryMessage {
        id,
        peer_id,
        from_id,
        sender_name: sender_name.to_owned(),
        date_unix,
        outgoing,
        is_post: false,
        text: parse_compose_markdown(body),
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
        extra,
    }
}
