//! Demo [`InMemoryExportSource`] dataset, optional step-pacing wrapper, and
//! [`TlExportSource`] adapter helpers for driving [`tg_export::ExportController`].
//!
//! Uses strictly fictional placeholder names and data (no real people's names or accounts).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tg_export::data::{
    ContactInfo, ContactsList, DialogInfo, DialogType, DialogsInfo, Document, File, Image, Media,
    MediaContent, Message, MessagesSlice, Peer, PersonalInfo, Photo, ProfileMusicSlice, Session,
    SessionsList, StoriesSlice, Story, TextPart, TopPeer, User, UserpicsSlice, WebSession,
};
use tg_export::output::stats::Stats;
use tg_export::stand_in::{InputPeer, InputUser, PeerId, UserId};
use tg_export::{
    ApiError, DownloadProgress, ExportRpc, ExportSource, InMemoryExportSource, Settings, StartInfo,
    StreamEvent, TlExportSource,
};
use tg_tl::types::{InputPeerChannel, InputPeerChat, InputPeerUser};

/// Shared skip-file request channel so the UI can ask the active [`ExportSource`]
/// to skip an in-flight download by its `random_id`.
#[derive(Debug, Clone, Default)]
pub struct SkipFileSignal {
    skipped_random_id: Arc<AtomicU64>,
}

impl SkipFileSignal {
    /// Creates a new skip-file signal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests skipping the file identified by `random_id`.
    pub fn request_skip(&self, random_id: u64) {
        self.skipped_random_id.store(random_id, Ordering::SeqCst);
    }

    /// Reads the latest requested skip `random_id` (`0` = none).
    #[must_use]
    pub fn current(&self) -> u64 {
        self.skipped_random_id.load(Ordering::SeqCst)
    }
}

/// Wrapper around any [`ExportSource`] (such as [`InMemoryExportSource`] or [`TlExportSource`])
/// that adds optional pacing delays for interactive demo visualization and forwards
/// [`SkipFileSignal`] requests cleanly.
pub struct PacedExportSource<S: ExportSource> {
    inner: S,
    step_delay: Duration,
    skip_signal: SkipFileSignal,
}

impl<S: ExportSource> PacedExportSource<S> {
    /// Creates a paced wrapper around `inner`. Pass `Duration::ZERO` in tests for instant runs.
    #[must_use]
    pub fn new(inner: S, step_delay: Duration, skip_signal: SkipFileSignal) -> Self {
        Self {
            inner,
            step_delay,
            skip_signal,
        }
    }

    fn sync_skip_signal(&mut self) {
        let id = self.skip_signal.current();
        if id != 0 {
            self.inner.skip_file(id);
        }
    }
}

impl<C: ExportRpc> PacedExportSource<TlExportSource<C>> {
    /// Wraps a live [`TlExportSource`] backed by any [`ExportRpc`] client.
    #[must_use]
    pub fn from_tl_rpc(rpc: C, utc_offset_secs: i32, skip_signal: SkipFileSignal) -> Self {
        Self::new(
            TlExportSource::with_utc_offset(rpc, utc_offset_secs),
            Duration::ZERO,
            skip_signal,
        )
    }
}

async fn maybe_pause(delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

impl<S: ExportSource> ExportSource for PacedExportSource<S> {
    async fn start_export(
        &mut self,
        settings: &Settings,
        stats: &Stats,
    ) -> Result<StartInfo, ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.start_export(settings, stats).await
    }

    async fn request_dialogs_list(
        &mut self,
        on_progress: &mut (dyn FnMut(i32) -> bool + Send),
    ) -> Result<DialogsInfo, ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_dialogs_list(on_progress).await
    }

    async fn request_personal_info(&mut self) -> Result<PersonalInfo, ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_personal_info().await
    }

    async fn request_userpics(
        &mut self,
        on_event: &mut (
                 dyn FnMut(StreamEvent<tg_export::data::UserpicsInfo, UserpicsSlice>) -> bool + Send
             ),
    ) -> Result<(), ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_userpics(on_event).await
    }

    async fn request_stories(
        &mut self,
        on_event: &mut (
                 dyn FnMut(StreamEvent<tg_export::data::StoriesInfo, StoriesSlice>) -> bool + Send
             ),
    ) -> Result<(), ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_stories(on_event).await
    }

    async fn request_profile_music(
        &mut self,
        on_event: &mut (
                 dyn FnMut(
            StreamEvent<tg_export::data::ProfileMusicInfo, ProfileMusicSlice>,
        ) -> bool
                     + Send
             ),
    ) -> Result<(), ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_profile_music(on_event).await
    }

    async fn request_contacts(&mut self) -> Result<ContactsList, ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_contacts().await
    }

    async fn request_sessions(&mut self) -> Result<SessionsList, ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_sessions().await
    }

    async fn request_other_data(&mut self, relative_path: &str) -> Result<File, ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_other_data(relative_path).await
    }

    async fn request_messages(
        &mut self,
        dialog: &DialogInfo,
        on_event: &mut (dyn FnMut(StreamEvent<DialogInfo, MessagesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner.request_messages(dialog, on_event).await
    }

    async fn request_topic_messages(
        &mut self,
        peer_id: PeerId,
        input_peer: &InputPeer,
        topic_root_id: i32,
        on_event: &mut (dyn FnMut(StreamEvent<i32, MessagesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        self.sync_skip_signal();
        maybe_pause(self.step_delay).await;
        self.inner
            .request_topic_messages(peer_id, input_peer, topic_root_id, on_event)
            .await
    }

    async fn finish_export(&mut self) -> Result<(), ApiError> {
        maybe_pause(self.step_delay).await;
        self.inner.finish_export().await
    }

    async fn finish_export_cancelled(&mut self) -> Result<(), ApiError> {
        self.inner.finish_export_cancelled().await
    }

    fn skip_file(&mut self, random_id: u64) {
        self.inner.skip_file(random_id);
    }

    fn cancel_export_fast(&mut self) {
        self.inner.cancel_export_fast();
    }
}

/// Builds a rich, realistic demo [`InMemoryExportSource`] populated with synthetic account data,
/// profile photos, archived stories, profile music, contacts, sessions, miscellaneous JSON, and
/// multi-chat histories with media attachments so running a demo export produces real HTML/JSON
/// files on disk.
///
/// Uses only fictional placeholder names (never any real person's name or real account data).
#[must_use]
pub fn build_demo_export_source(single_peer: Option<(&InputPeer, &str)>) -> InMemoryExportSource {
    let self_user = User {
        bare_id: 100_001,
        info: ContactInfo {
            user_id: UserId(100_001),
            first_name: "Demo".to_owned(),
            last_name: "Account".to_owned(),
            phone_number: "+10005550100".to_owned(),
            date: 1_700_000_000,
            color_index: 5,
        },
        username: "demo_account".to_owned(),
        color_index: 5,
        is_self: true,
        input: InputUser::Self_,
        ..Default::default()
    };

    let peer_alpha = User {
        bare_id: 100_002,
        info: ContactInfo {
            user_id: UserId(100_002),
            first_name: "Orbit".to_owned(),
            last_name: "Tester".to_owned(),
            phone_number: "+10005550101".to_owned(),
            date: 1_700_010_000,
            color_index: 1,
        },
        username: "orbit_tester".to_owned(),
        color_index: 1,
        ..Default::default()
    };

    let peer_bot = User {
        bare_id: 100_003,
        info: ContactInfo {
            user_id: UserId(100_003),
            first_name: "Release".to_owned(),
            last_name: "Bot".to_owned(),
            phone_number: String::new(),
            date: 1_700_020_000,
            color_index: 3,
        },
        username: "release_demo_bot".to_owned(),
        color_index: 3,
        is_bot: true,
        ..Default::default()
    };

    let self_id = self_user.id();
    let alpha_id = peer_alpha.id();
    let bot_id = peer_bot.id();
    let group_id = PeerId::from_chat(tg_export::stand_in::ChatId(200_001));
    let channel_id = PeerId::from_channel(tg_export::stand_in::ChannelId(300_001));

    let mut peers_map = BTreeMap::new();
    peers_map.insert(self_id, Peer::User(self_user.clone()));
    peers_map.insert(alpha_id, Peer::User(peer_alpha.clone()));
    peers_map.insert(bot_id, Peer::User(peer_bot.clone()));

    let tiny_png: Vec<u8> = vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 96, 248, 207, 192,
        240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    let mut files_to_write = BTreeMap::new();
    files_to_write.insert("profile_pictures/photo_1.png".to_owned(), tiny_png.clone());
    files_to_write.insert("stories/story_1.png".to_owned(), tiny_png.clone());
    files_to_write.insert(
        "chats/chat_1/photos/photo_1.png".to_owned(),
        tiny_png.clone(),
    );
    files_to_write.insert(
        "chats/chat_2/files/release_notes.txt".to_owned(),
        b"Rustgram demo export release checklist.\nAll checks passing.\n".to_vec(),
    );

    let userpic_slices = vec![UserpicsSlice {
        list: vec![Photo {
            id: 9001,
            date: 1_700_100_000,
            spoilered: false,
            image: Image {
                width: 640,
                height: 640,
                file: File {
                    size: tiny_png.len() as i64,
                    relative_path: "profile_pictures/photo_1.png".to_owned(),
                    suggested_path: "profile_pictures/photo_1.png".to_owned(),
                    ..Default::default()
                },
            },
        }],
    }];

    let story_slices = vec![StoriesSlice {
        list: vec![Story {
            id: 701,
            date: 1_701_000_000,
            expires: 1_701_086_400,
            media: Media {
                content: MediaContent::Photo(Photo {
                    id: 9002,
                    date: 1_701_000_000,
                    spoilered: false,
                    image: Image {
                        width: 1080,
                        height: 1920,
                        file: File {
                            size: tiny_png.len() as i64,
                            relative_path: "stories/story_1.png".to_owned(),
                            suggested_path: "stories/story_1.png".to_owned(),
                            ..Default::default()
                        },
                    },
                }),
                ttl: 0,
            },
            pinned: true,
            caption: vec![TextPart::plain("Demo workspace milestone snapshot")],
        }],
        last_id: 701,
        skipped: 0,
    }];

    let profile_music_slices = vec![ProfileMusicSlice {
        list: vec![Message {
            id: 501,
            date: 1_701_200_000,
            from_id: self_id,
            peer_id: self_id,
            self_id,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 8001,
                    date: 1_701_200_000,
                    name: "ambient_loop.mp3".to_owned(),
                    mime: "audio/mpeg".to_owned(),
                    song_performer: "Synth Lab".to_owned(),
                    song_title: "Focus Drift".to_owned(),
                    duration: 184,
                    is_audio_file: true,
                    file: File {
                        size: 245_760,
                        relative_path: "profile_music/ambient_loop.mp3".to_owned(),
                        suggested_path: "profile_music/ambient_loop.mp3".to_owned(),
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        }],
        skipped: 0,
    }];

    let contacts = ContactsList {
        list: vec![peer_alpha.info.clone()],
        correspondents: vec![TopPeer {
            peer: Peer::User(peer_alpha.clone()),
            rating: 12.5,
        }],
        inline_bots: vec![TopPeer {
            peer: Peer::User(peer_bot.clone()),
            rating: 4.0,
        }],
        phone_calls: Vec::new(),
    };

    let sessions = SessionsList {
        list: vec![Session {
            application_id: 2040,
            platform: "Windows 11 x64".to_owned(),
            device_model: "Desktop".to_owned(),
            system_version: "Windows 11".to_owned(),
            application_name: "Rustgram Desktop".to_owned(),
            application_version: "0.1.0".to_owned(),
            created: 1_700_000_000,
            last_active: 1_705_000_000,
            ip: "127.0.0.1".to_owned(),
            country: "Local".to_owned(),
            region: "Demo".to_owned(),
        }],
        web_list: vec![WebSession {
            bot_username: "release_demo_bot".to_owned(),
            domain: "example.invalid".to_owned(),
            browser: "Chromium".to_owned(),
            platform: "Windows".to_owned(),
            created: 1_700_500_000,
            last_active: 1_704_000_000,
            ip: "127.0.0.1".to_owned(),
            region: "Demo".to_owned(),
        }],
    };

    let dialog_saved = DialogInfo {
        kind: DialogType::SelfChat,
        name: "Saved Messages".to_owned(),
        input: InputPeer::Self_,
        top_message_id: 102,
        top_message_date: 1_705_100_000,
        peer_id: self_id,
        color_index: 5,
        messages_count_per_split: vec![2],
        ..Default::default()
    };

    let dialog_alpha = DialogInfo {
        kind: DialogType::Personal,
        name: "Orbit".to_owned(),
        last_name: "Tester".to_owned(),
        input: InputPeer::User(InputPeerUser {
            user_id: 100_002,
            access_hash: 42,
        }),
        top_message_id: 203,
        top_message_date: 1_705_200_000,
        peer_id: alpha_id,
        color_index: 1,
        messages_count_per_split: vec![3],
        ..Default::default()
    };

    let dialog_group = DialogInfo {
        kind: DialogType::PrivateGroup,
        name: "Design Notes".to_owned(),
        input: InputPeer::Chat(InputPeerChat { chat_id: 200_001 }),
        top_message_id: 302,
        top_message_date: 1_705_300_000,
        peer_id: group_id,
        color_index: 2,
        messages_count_per_split: vec![2],
        ..Default::default()
    };

    let dialog_channel = DialogInfo {
        kind: DialogType::PublicChannel,
        name: "Workspace Updates".to_owned(),
        input: InputPeer::Channel(InputPeerChannel {
            channel_id: 300_001,
            access_hash: 99,
        }),
        top_message_id: 401,
        top_message_date: 1_705_400_000,
        peer_id: channel_id,
        color_index: 4,
        only_my_messages: true,
        messages_count_per_split: vec![1],
        ..Default::default()
    };

    let saved_messages = vec![MessagesSlice {
        list: vec![
            Message {
                id: 101,
                date: 1_705_000_000,
                from_id: self_id,
                peer_id: self_id,
                self_id,
                out: true,
                text: vec![TextPart::plain(
                    "Remember to verify HTML and JSON export formatting.",
                )],
                ..Default::default()
            },
            Message {
                id: 102,
                date: 1_705_100_000,
                from_id: self_id,
                peer_id: self_id,
                self_id,
                out: true,
                text: vec![TextPart::plain("Attached sample preview photo.")],
                media: Media {
                    content: MediaContent::Photo(Photo {
                        id: 9101,
                        date: 1_705_100_000,
                        spoilered: false,
                        image: Image {
                            width: 1280,
                            height: 720,
                            file: File {
                                size: tiny_png.len() as i64,
                                relative_path: "chats/chat_1/photos/photo_1.png".to_owned(),
                                suggested_path: "chats/chat_1/photos/photo_1.png".to_owned(),
                                ..Default::default()
                            },
                        },
                    }),
                    ttl: 0,
                },
                ..Default::default()
            },
        ],
        peers: peers_map.clone(),
    }];

    let alpha_messages = vec![MessagesSlice {
        list: vec![
            Message {
                id: 201,
                date: 1_705_150_000,
                from_id: alpha_id,
                peer_id: alpha_id,
                self_id,
                text: vec![TextPart::plain("Hey! Ready to test the local chat export?")],
                ..Default::default()
            },
            Message {
                id: 202,
                date: 1_705_160_000,
                from_id: self_id,
                peer_id: alpha_id,
                self_id,
                out: true,
                text: vec![TextPart::plain(
                    "Yes — exporting with HTML + JSON writers from tg-export.",
                )],
                ..Default::default()
            },
            Message {
                id: 203,
                date: 1_705_200_000,
                from_id: alpha_id,
                peer_id: alpha_id,
                self_id,
                text: vec![TextPart::plain("Here is the release notes document.")],
                media: Media {
                    content: MediaContent::Document(Box::new(Document {
                        id: 8101,
                        date: 1_705_200_000,
                        name: "release_notes.txt".to_owned(),
                        mime: "text/plain".to_owned(),
                        file: File {
                            size: 62,
                            relative_path: "chats/chat_2/files/release_notes.txt".to_owned(),
                            suggested_path: "chats/chat_2/files/release_notes.txt".to_owned(),
                            ..Default::default()
                        },
                        ..Default::default()
                    })),
                    ttl: 0,
                },
                ..Default::default()
            },
        ],
        peers: peers_map.clone(),
    }];

    let group_messages = vec![MessagesSlice {
        list: vec![
            Message {
                id: 301,
                date: 1_705_250_000,
                from_id: alpha_id,
                peer_id: group_id,
                self_id,
                text: vec![TextPart::plain(
                    "Updated export panel spacing to match export.style.",
                )],
                ..Default::default()
            },
            Message {
                id: 302,
                date: 1_705_300_000,
                from_id: self_id,
                peer_id: group_id,
                self_id,
                out: true,
                text: vec![TextPart::plain(
                    "Confirmed: size slider spans 1 MB to 4000 MB in 100 steps.",
                )],
                ..Default::default()
            },
        ],
        peers: peers_map.clone(),
    }];

    let channel_messages = vec![MessagesSlice {
        list: vec![Message {
            id: 401,
            date: 1_705_400_000,
            from_id: self_id,
            peer_id: channel_id,
            self_id,
            out: true,
            text: vec![TextPart::plain("Build v0.1.0 changelog published.")],
            ..Default::default()
        }],
        peers: peers_map.clone(),
    }];

    let (dialogs, dialog_messages, topic_messages) = if let Some((peer, title)) = single_peer {
        let single_peer_id = peer_id_from_input_peer(peer, self_id);
        let single_dialog = DialogInfo {
            kind: match peer {
                InputPeer::Self_ => DialogType::SelfChat,
                InputPeer::User(_) | InputPeer::UserFromMessage(_) => DialogType::Personal,
                InputPeer::Chat(_) => DialogType::PrivateGroup,
                InputPeer::Channel(_) | InputPeer::ChannelFromMessage(_) => {
                    DialogType::PublicSupergroup
                }
                InputPeer::Empty => DialogType::Personal,
            },
            name: if title.is_empty() {
                "Demo Chat".to_owned()
            } else {
                title.to_owned()
            },
            input: peer.clone(),
            top_message_id: 203,
            top_message_date: 1_705_200_000,
            peer_id: single_peer_id,
            color_index: 1,
            messages_count_per_split: vec![3],
            ..Default::default()
        };
        let mut map = BTreeMap::new();
        let mut single_slices = alpha_messages.clone();
        for slice in &mut single_slices {
            for msg in &mut slice.list {
                msg.peer_id = single_peer_id;
            }
        }
        map.insert(single_peer_id, single_slices.clone());
        (
            DialogsInfo {
                chats: vec![single_dialog],
                left: Vec::new(),
            },
            map,
            single_slices,
        )
    } else {
        let mut map = BTreeMap::new();
        map.insert(self_id, saved_messages);
        map.insert(alpha_id, alpha_messages.clone());
        map.insert(group_id, group_messages);
        map.insert(channel_id, channel_messages);
        (
            DialogsInfo {
                chats: vec![dialog_saved, dialog_alpha, dialog_group, dialog_channel],
                left: Vec::new(),
            },
            map,
            alpha_messages,
        )
    };

    let mut src = InMemoryExportSource::default();
    src.personal_info = PersonalInfo {
        user: self_user,
        bio: "Local Rustgram demo account for data export testing.".to_owned(),
    };
    src.userpic_slices = userpic_slices;
    src.story_slices = story_slices;
    src.profile_music_slices = profile_music_slices;
    src.contacts = contacts;
    src.sessions = sessions;
    src.other_data_json =
        Some(br#"{"demo_version":1,"notes":"Synthetic miscellaneous export payload"}"#.to_vec());
    src.dialogs = dialogs;
    src.dialog_messages = dialog_messages;
    src.topic_messages = topic_messages;
    src.files_to_write = files_to_write;
    src
}

/// Converts a TL [`InputPeer`] into a `tg_data::PeerId` (port of `ReadPeerId` in `export_view_settings.cpp`).
#[must_use]
pub fn peer_id_from_input_peer(input: &InputPeer, self_peer_id: PeerId) -> PeerId {
    match input {
        InputPeer::User(u) => PeerId::from_user(UserId(u.user_id as u64)),
        InputPeer::UserFromMessage(u) => PeerId::from_user(UserId(u.user_id as u64)),
        InputPeer::Chat(c) => PeerId::from_chat(tg_export::stand_in::ChatId(c.chat_id as u64)),
        InputPeer::Channel(c) => {
            PeerId::from_channel(tg_export::stand_in::ChannelId(c.channel_id as u64))
        }
        InputPeer::ChannelFromMessage(c) => {
            PeerId::from_channel(tg_export::stand_in::ChannelId(c.channel_id as u64))
        }
        InputPeer::Self_ => self_peer_id,
        InputPeer::Empty => PeerId::ZERO,
    }
}

/// Helper verifying [`DownloadProgress`] formatting in unit tests and progress previews.
#[must_use]
pub fn sample_download_progress() -> DownloadProgress {
    DownloadProgress {
        random_id: 42_001,
        path: "chats/chat_02/photos/photo_14@18-01-2026_12-30-00.jpg".to_owned(),
        item_index: 14,
        ready: 1_572_864,
        total: 3_670_016,
    }
}
