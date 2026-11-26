//! Golden tests for `tg-export` JSON and HTML writers, pagination, escaping,
//! file naming/collision rules, and Qt settings serialization.
//!
//! All sample names, phone numbers, and payloads are strictly fictional.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use image::{ImageBuffer, ImageFormat, Rgba};
use serde_json::Value;

use tg_export::controller::{ExportController, ExportState, InMemoryExportSource};
use tg_export::data::{
    Chat, ContactInfo, ContactsList, DialogInfo, DialogType, DialogsInfo, Document, File, Game,
    GeoPoint, GiveawayResults, GiveawayStart, HistoryMessageMarkupButton, Image,
    InlineButtonAction, InlineButtonActionType, Invoice, MarkupButtonType, Media, MediaContent,
    Message, MessagesSlice, PaidMedia, Peer, PersonalInfo, PhoneCallState, Photo, Poll, PollAnswer,
    ProfileMusicSlice, Reaction, ReactionRecent, ReactionType, RichBlock, RichBlockKind,
    RichButtonAlignment, RichButtonPayload, RichButtonStyle, RichCaption, RichListItem,
    RichListItemContent, RichListKind, RichMessage, RichOrderedList, RichQuoteContent,
    RichTableAlignment, RichTableCell, RichTableRow, RichTableVerticalAlignment, RichTaskState,
    RichText, RichTextType, SecureValueType, ServiceAction, Session, SessionsList, SharedContact,
    SkipReason, StoriesSlice, Story, TextPart, TextPartType, TodoList, TodoListItem, TopPeer,
    UnsupportedMedia, User, UserpicsSlice, Venue, WebSession, finalize_dialogs_info,
};
use tg_export::output::assets::EXPORT_HTML_ASSETS;
use tg_export::output::file::OutputFile;
use tg_export::output::html::{MESSAGES_IN_FILE, serialize_html_string};
use tg_export::output::json::serialize_json_string;
use tg_export::output::{OutputFormat, normalize_path_with_date};
use tg_export::settings::{Environment, ExportTypes, MediaSettings, MediaTypes, Settings};
use tg_export::stand_in::{
    Birthday, ChannelId, CreditsAmount, CreditsType, InputPeer, PeerId, UserId,
    file_name_from_user_string,
};
use tg_tl::types::{InputPeerChannel, InputPeerChat, InputPeerUser};

static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(1);

fn temp_export_dir(label: &str) -> PathBuf {
    let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "tg_export_golden_{}_{}_{}",
        label,
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn dir_with_slash(dir: &Path) -> String {
    let mut s = dir.to_string_lossy().replace('\\', "/");
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

/// Encodes a valid solid-color PNG of `w x h` pixels for thumbnail tests.
fn make_png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(w, h, Rgba(rgba));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}

fn write_fixture_file(root: &Path, rel: &str, bytes: &[u8]) {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(full, bytes).expect("write fixture file");
}

fn build_user(
    id: u64,
    first: &str,
    last: &str,
    username: &str,
    phone: &str,
    is_bot: bool,
    is_self: bool,
) -> User {
    User {
        bare_id: id,
        info: ContactInfo {
            user_id: UserId(id),
            first_name: first.to_string(),
            last_name: last.to_string(),
            phone_number: phone.to_string(),
            date: 1_700_000_000,
            color_index: (id % 7) as u8,
        },
        username: username.to_string(),
        color_index: (id % 7) as u8,
        is_bot,
        is_self,
        ..Default::default()
    }
}

fn build_chat(
    id: u64,
    title: &str,
    username: &str,
    is_broadcast: bool,
    is_supergroup: bool,
) -> Chat {
    Chat {
        bare_id: id,
        title: title.to_string(),
        username: username.to_string(),
        color_index: (id % 7) as u8,
        is_broadcast,
        is_supergroup,
        ..Default::default()
    }
}

fn build_comprehensive_source(export_root: &Path) -> (InMemoryExportSource, Settings, Environment) {
    let self_user = build_user(
        1001,
        "Alden",
        "Vance",
        "alden_v",
        "15550101001",
        false,
        true,
    );
    let peer_briar = build_user(
        1002,
        "Briar",
        "Moss",
        "briar_m",
        "15550101002",
        false,
        false,
    );
    let peer_bot = build_user(1003, "Cinder", "Bot", "cinder_test_bot", "", true, false);
    let peer_dusk = build_user(1004, "Dusk", "Vale", "dusk_v", "447700900123", false, false);

    let chat_priv_group = build_chat(2001, "Lantern Squad", "", false, false);
    let chat_priv_super = build_chat(3001, "Quartz Lounge", "", false, true);
    let chat_pub_super = build_chat(3002, "Harbor Forum", "harbor_forum", false, true);
    let chat_priv_chan = build_chat(3003, "Private Dispatch", "", true, false);
    let chat_pub_chan = build_chat(3004, "Beacon Broadcast", "beacon_news", true, false);
    let chat_left_chan = build_chat(3005, "Archived Beacon", "old_beacon", true, false);

    let self_id = self_user.id();
    let briar_id = peer_briar.id();
    let bot_id = peer_bot.id();
    let dusk_id = peer_dusk.id();

    let mut peers: BTreeMap<PeerId, Peer> = BTreeMap::new();
    for u in [&self_user, &peer_briar, &peer_bot, &peer_dusk] {
        peers.insert(u.id(), Peer::User(u.clone()));
    }
    for c in [
        &chat_priv_group,
        &chat_priv_super,
        &chat_pub_super,
        &chat_priv_chan,
        &chat_pub_chan,
        &chat_left_chan,
    ] {
        peers.insert(c.id(), Peer::Chat(c.clone()));
    }

    // Write real image/media fixture files on disk so thumbnailing & relative paths work end-to-end.
    let png_large = make_png_bytes(600, 400, [40, 120, 200, 255]);
    let png_sticker = make_png_bytes(512, 512, [220, 80, 90, 255]);
    let png_thumb = make_png_bytes(160, 160, [60, 180, 90, 255]);
    write_fixture_file(
        export_root,
        "profile_pictures/photo_1@14-11-2023_22-13-20.png",
        &png_large,
    );
    write_fixture_file(
        export_root,
        "stories/story_1@14-11-2023_22-15-00.png",
        &png_large,
    );
    write_fixture_file(
        export_root,
        "stories/story_2@14-11-2023_22-16-00.mp4",
        b"fake-story-mp4",
    );
    write_fixture_file(
        export_root,
        "stories/story_2@14-11-2023_22-16-00.mp4_thumb.png",
        &png_thumb,
    );
    write_fixture_file(export_root, "profile_music/audio_1.mp3", b"fake-mp3-data");
    write_fixture_file(
        export_root,
        "other_data.json",
        b"{\"ip_history\":[\"198.51.100.1\"]}\n",
    );
    write_fixture_file(export_root, "stickers/custom_emoji_1.png", &png_thumb);
    write_fixture_file(export_root, "chats/chat_2/photos/photo_1.png", &png_large);
    write_fixture_file(export_root, "chats/chat_2/photos/rich_1.png", &png_large);
    write_fixture_file(export_root, "chats/chat_2/photos/rich_2.png", &png_sticker);
    write_fixture_file(export_root, "chats/chat_2/photos/rich_3.png", &png_thumb);
    write_fixture_file(
        export_root,
        "chats/chat_2/stickers/sticker_1.png",
        &png_sticker,
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/files/report.pdf",
        b"%PDF-1.4\nfake",
    );
    write_fixture_file(export_root, "chats/chat_2/files/track.mp3", b"ID3fakeaudio");
    write_fixture_file(
        export_root,
        "chats/chat_2/voice_messages/voice_1.ogg",
        b"OggSfakevoice",
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/video_files/clip.mp4",
        b"fakevideo",
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/video_files/clip.mp4_thumb.png",
        &png_thumb,
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/round_video_messages/round_1.mp4",
        b"fakeround",
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/round_video_messages/round_1.mp4_thumb.png",
        &png_thumb,
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/animations/anim_1.mp4",
        b"fakegifmp4",
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/animations/anim_1.mp4_thumb.png",
        &png_thumb,
    );
    write_fixture_file(
        export_root,
        "chats/chat_2/contacts/contact_1.vcard",
        b"BEGIN:VCARD\nFN:Dusk Vale\nEND:VCARD\n",
    );

    let mut settings = Settings {
        path: dir_with_slash(export_root),
        format: OutputFormat::HtmlAndJson,
        types: ExportTypes::PERSONAL_INFO
            | ExportTypes::USERPICS
            | ExportTypes::STORIES
            | ExportTypes::PROFILE_MUSIC
            | ExportTypes::CONTACTS
            | ExportTypes::SESSIONS
            | ExportTypes::OTHER_DATA
            | ExportTypes::PERSONAL_CHATS
            | ExportTypes::BOT_CHATS
            | ExportTypes::PRIVATE_GROUPS
            | ExportTypes::PRIVATE_CHANNELS
            | ExportTypes::PUBLIC_GROUPS
            | ExportTypes::PUBLIC_CHANNELS,
        full_chats: ExportTypes::PERSONAL_CHATS
            | ExportTypes::BOT_CHATS
            | ExportTypes::PRIVATE_GROUPS
            | ExportTypes::PRIVATE_CHANNELS,
        media: MediaSettings {
            types: MediaTypes::ALL_MASK,
            size_limit: 8 * 1024 * 1024,
        },
        ..Default::default()
    };

    let environment = Environment::default_english();

    let mut dialogs = DialogsInfo {
        chats: vec![
            DialogInfo {
                kind: DialogType::SelfChat,
                name: self_user.info.first_name.clone(),
                last_name: self_user.info.last_name.clone(),
                peer_id: self_id,
                color_index: self_user.color_index,
                ..Default::default()
            },
            DialogInfo {
                kind: DialogType::Personal,
                name: peer_briar.info.first_name.clone(),
                last_name: peer_briar.info.last_name.clone(),
                peer_id: briar_id,
                color_index: peer_briar.color_index,
                ..Default::default()
            },
            DialogInfo {
                kind: DialogType::Bot,
                name: peer_bot.info.first_name.clone(),
                last_name: peer_bot.info.last_name.clone(),
                peer_id: bot_id,
                color_index: peer_bot.color_index,
                ..Default::default()
            },
            DialogInfo {
                kind: DialogType::PrivateGroup,
                name: chat_priv_group.title.clone(),
                peer_id: chat_priv_group.id(),
                color_index: chat_priv_group.color_index,
                ..Default::default()
            },
            DialogInfo {
                kind: DialogType::PrivateSupergroup,
                name: chat_priv_super.title.clone(),
                peer_id: chat_priv_super.id(),
                color_index: chat_priv_super.color_index,
                ..Default::default()
            },
            DialogInfo {
                kind: DialogType::PublicSupergroup,
                name: chat_pub_super.title.clone(),
                peer_id: chat_pub_super.id(),
                color_index: chat_pub_super.color_index,
                ..Default::default()
            },
            DialogInfo {
                kind: DialogType::PrivateChannel,
                name: chat_priv_chan.title.clone(),
                peer_id: chat_priv_chan.id(),
                color_index: chat_priv_chan.color_index,
                ..Default::default()
            },
            DialogInfo {
                kind: DialogType::PublicChannel,
                name: chat_pub_chan.title.clone(),
                peer_id: chat_pub_chan.id(),
                color_index: chat_pub_chan.color_index,
                ..Default::default()
            },
        ],
        left: vec![DialogInfo {
            kind: DialogType::PublicChannel,
            name: chat_left_chan.title.clone(),
            peer_id: chat_left_chan.id(),
            color_index: chat_left_chan.color_index,
            is_left_channel: true,
            only_my_messages: true,
            ..Default::default()
        }],
    };
    finalize_dialogs_info(&mut dialogs, &settings);

    // Base timestamp: 2023-11-14T22:13:20Z = 1_700_000_000
    let t0 = 1_700_000_000_i32;

    // Chat 1: Saved Messages (forwarded with saved_from & hidden forward name)
    let saved_msgs = vec![
        Message {
            id: 1,
            date: t0,
            from_id: self_id,
            peer_id: self_id,
            self_id,
            forwarded: true,
            forwarded_from_id: briar_id,
            forwarded_date: t0 - 300,
            show_forwarded_as_original: true,
            saved_from_chat_id: briar_id,
            out: true,
            text: vec![TextPart::plain("Saved note from Briar")],
            ..Default::default()
        },
        Message {
            id: 2,
            date: t0 + 10,
            from_id: self_id,
            peer_id: self_id,
            self_id,
            forwarded: true,
            forwarded_from_name: "Hidden Sender <Anon>".to_string(),
            forwarded_date: t0 - 100,
            show_forwarded_as_original: true,
            out: true,
            text: vec![TextPart::plain("Hidden forward")],
            ..Default::default()
        },
    ];

    // Chat 2: Personal chat with Briar covering EVERY media type, EVERY text entity,
    // replies (same-chat & cross-chat), forwards, edited, reactions, inline buttons, rich message.
    let all_entities_text = vec![
        TextPart::plain("Plain & <\"'> "),
        TextPart::with_type(TextPartType::Unknown, "unk", ""),
        TextPart::with_type(TextPartType::Mention, "@briar_m", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Hashtag, "#export_test", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::BotCommand, "/start", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Url, "https://example.org/path?a=1&b=2", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Email, "test@example.invalid", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Bold, "bold", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Italic, "italic", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Code, "let x = 1;", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Pre, "fn main() {}\n", "rust"),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::TextUrl, "click", "https://example.org/docs"),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::MentionName, "Dusk", "1004"),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Phone, "+15550101002", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Cashtag, "$TEST", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Underline, "under", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Strike, "strike", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Blockquote, "quoted line", ""),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Blockquote, "collapsed quote", "1"),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::BankCard, "4000-0000-0000-0000", ""),
        TextPart::plain(" "),
        TextPart::with_type(
            TextPartType::TonAddress,
            "EQTestAddress0000000000000000000000000000000000",
            "",
        ),
        TextPart::plain(" "),
        TextPart::with_type(TextPartType::Spoiler, "secret", ""),
        TextPart::plain(" "),
        TextPart::with_type(
            TextPartType::CustomEmoji,
            "\u{2B50}",
            "stickers/custom_emoji_1.png",
        ),
        TextPart::with_type(TextPartType::CustomEmoji, "\u{1F525}", ""),
        TextPart::with_type(
            TextPartType::CustomEmoji,
            "\u{2753}",
            TextPart::UNAVAILABLE_EMOJI,
        ),
    ];

    let mut rich_photos = BTreeMap::new();
    for (pid, rel, w, h) in [
        (901_u64, "chats/chat_2/photos/rich_1.png", 600, 400),
        (902_u64, "chats/chat_2/photos/rich_2.png", 512, 512),
        (903_u64, "chats/chat_2/photos/rich_3.png", 160, 160),
    ] {
        rich_photos.insert(
            pid,
            Photo {
                id: pid,
                date: t0,
                spoilered: false,
                image: Image {
                    width: w,
                    height: h,
                    file: File {
                        relative_path: rel.to_string(),
                        size: 512,
                        ..Default::default()
                    },
                },
            },
        );
    }
    let rich_message = RichMessage {
        rtl: false,
        part: false,
        photos: rich_photos,
        documents: BTreeMap::new(),
        blocks: vec![
            RichBlock {
                kind: RichBlockKind::Heading,
                heading_level: 2,
                text: RichText {
                    kind: RichTextType::Plain,
                    text: "Rich Summary".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::Paragraph,
                text: RichText {
                    kind: RichTextType::Concat,
                    children: vec![
                        RichText {
                            kind: RichTextType::Bold,
                            children: vec![RichText {
                                kind: RichTextType::Plain,
                                text: "Notice:".to_string(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        RichText {
                            kind: RichTextType::Plain,
                            text: " equation ".to_string(),
                            ..Default::default()
                        },
                        RichText {
                            kind: RichTextType::Math,
                            data: "E=mc^2".to_string(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::Code,
                language: "rust".to_string(),
                text: RichText {
                    kind: RichTextType::Plain,
                    text: "println!(\"ok\");".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::Quote,
                quote_content: RichQuoteContent::Text,
                text: RichText {
                    kind: RichTextType::Plain,
                    text: "Keep exports faithful.".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::List,
                list_kind: RichListKind::Ordered,
                ordered_list: RichOrderedList {
                    start: Some(1),
                    ..Default::default()
                },
                list_items: vec![RichListItem {
                    content: RichListItemContent::Text,
                    task_state: RichTaskState::Checked,
                    text: Some(RichText {
                        kind: RichTextType::Plain,
                        text: "Verified".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::Table,
                bordered: true,
                striped: true,
                table_rows: vec![RichTableRow {
                    cells: vec![
                        RichTableCell {
                            header: true,
                            alignment: RichTableAlignment::Center,
                            vertical_alignment: RichTableVerticalAlignment::Middle,
                            text: Some(RichText {
                                kind: RichTextType::Plain,
                                text: "Col A".to_string(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        RichTableCell {
                            header: false,
                            text: Some(RichText {
                                kind: RichTextType::Plain,
                                text: "Val 1".to_string(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                }],
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::Collage,
                blocks: vec![
                    RichBlock {
                        kind: RichBlockKind::Photo,
                        photo_id: 901,
                        caption: RichCaption::default(),
                        ..Default::default()
                    },
                    RichBlock {
                        kind: RichBlockKind::Photo,
                        photo_id: 902,
                        ..Default::default()
                    },
                    RichBlock {
                        kind: RichBlockKind::Photo,
                        photo_id: 903,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::ButtonRow,
                button_alignment: RichButtonAlignment::Center,
                buttons: vec![RichText {
                    kind: RichTextType::Button,
                    children: vec![RichText {
                        kind: RichTextType::Plain,
                        text: "Open".to_string(),
                        ..Default::default()
                    }],
                    button: Some(Box::new(RichButtonPayload {
                        action: InlineButtonAction {
                            kind: InlineButtonActionType::Url,
                            url: "https://example.org".to_string(),
                            ..Default::default()
                        },
                        style: Some(RichButtonStyle::Primary),
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::Divider,
                ..Default::default()
            },
            RichBlock {
                kind: RichBlockKind::Anchor,
                name: "sec_end".to_string(),
                ..Default::default()
            },
        ],
    };

    let personal_msgs = vec![
        // 1: All text entities + edited + reactions (emoji, custom emoji, paid) + inline buttons
        Message {
            id: 10,
            date: t0,
            edited: t0 + 45,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            text: all_entities_text,
            reactions: vec![
                Reaction {
                    kind: ReactionType::Emoji,
                    emoji: "\u{1F44D}".to_string(),
                    count: 2,
                    recent: vec![
                        ReactionRecent {
                            peer_id: self_id,
                            date: t0 + 10,
                        },
                        ReactionRecent {
                            peer_id: briar_id,
                            date: t0 + 20,
                        },
                    ],
                    ..Default::default()
                },
                Reaction {
                    kind: ReactionType::CustomEmoji,
                    document_id: "stickers/custom_emoji_1.png".to_string(),
                    count: 1,
                    ..Default::default()
                },
                Reaction {
                    kind: ReactionType::Paid,
                    count: 5,
                    ..Default::default()
                },
            ],
            inline_button_rows: vec![vec![
                HistoryMessageMarkupButton {
                    kind: MarkupButtonType::Url,
                    text: "Visit Docs".to_string(),
                    data: b"https://example.org/docs".to_vec(),
                    ..Default::default()
                },
                HistoryMessageMarkupButton {
                    kind: MarkupButtonType::Callback,
                    text: "Ack".to_string(),
                    data: b"ack_10".to_vec(),
                    ..Default::default()
                },
            ]],
            ..Default::default()
        },
        // 2: Reply (same-chat) + normal photo (spoiler)
        Message {
            id: 11,
            date: t0 + 60,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            reply_to_msg_id: 10,
            media: Media {
                content: MediaContent::Photo(Photo {
                    id: 5001,
                    date: t0 + 60,
                    spoilered: true,
                    image: Image {
                        width: 600,
                        height: 400,
                        file: File {
                            relative_path: "chats/chat_2/photos/photo_1.png".to_string(),
                            size: png_large.len() as i64,
                            ..Default::default()
                        },
                    },
                }),
                ttl: 0,
            },
            text: vec![TextPart::plain("Spoiler photo")],
            ..Default::default()
        },
        // 3: Cross-chat reply + self-destructing photo (TTL)
        Message {
            id: 12,
            date: t0 + 70,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            reply_to_msg_id: 99,
            reply_to_peer_id: chat_priv_group.id(),
            media: Media {
                content: MediaContent::Photo(Photo {
                    id: 5002,
                    date: t0 + 70,
                    ..Default::default()
                }),
                ttl: 10,
            },
            ..Default::default()
        },
        // 4: Generic document file
        Message {
            id: 13,
            date: t0 + 80,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6001,
                    date: t0 + 80,
                    name: "report.pdf".to_string(),
                    mime: "application/pdf".to_string(),
                    file: File {
                        relative_path: "chats/chat_2/files/report.pdf".to_string(),
                        size: 13,
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
        // 5: Audio file
        Message {
            id: 14,
            date: t0 + 90,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6002,
                    date: t0 + 90,
                    name: "track.mp3".to_string(),
                    mime: "audio/mpeg".to_string(),
                    song_performer: "Synth Trio".to_string(),
                    song_title: "Night Signal".to_string(),
                    duration: 185,
                    is_audio_file: true,
                    file: File {
                        relative_path: "chats/chat_2/files/track.mp3".to_string(),
                        size: 12,
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
        // 6: Voice message
        Message {
            id: 15,
            date: t0 + 100,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6003,
                    date: t0 + 100,
                    mime: "audio/ogg".to_string(),
                    duration: 14,
                    is_voice_message: true,
                    file: File {
                        relative_path: "chats/chat_2/voice_messages/voice_1.ogg".to_string(),
                        size: 13,
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
        // 7: Video file with thumb
        Message {
            id: 16,
            date: t0 + 110,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6004,
                    date: t0 + 110,
                    name: "clip.mp4".to_string(),
                    mime: "video/mp4".to_string(),
                    width: 640,
                    height: 360,
                    duration: 42,
                    is_video_file: true,
                    file: File {
                        relative_path: "chats/chat_2/video_files/clip.mp4".to_string(),
                        size: 9,
                        ..Default::default()
                    },
                    thumb: Image {
                        width: 160,
                        height: 160,
                        file: File {
                            relative_path: "chats/chat_2/video_files/clip.mp4_thumb.png"
                                .to_string(),
                            size: png_thumb.len() as i64,
                            ..Default::default()
                        },
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
        // 8: Round video message
        Message {
            id: 17,
            date: t0 + 120,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6005,
                    date: t0 + 120,
                    mime: "video/mp4".to_string(),
                    width: 240,
                    height: 240,
                    duration: 7,
                    is_video_message: true,
                    file: File {
                        relative_path: "chats/chat_2/round_video_messages/round_1.mp4".to_string(),
                        size: 9,
                        ..Default::default()
                    },
                    thumb: Image {
                        width: 160,
                        height: 160,
                        file: File {
                            relative_path:
                                "chats/chat_2/round_video_messages/round_1.mp4_thumb.png"
                                    .to_string(),
                            size: png_thumb.len() as i64,
                            ..Default::default()
                        },
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
        // 9: Sticker
        Message {
            id: 18,
            date: t0 + 130,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6006,
                    date: t0 + 130,
                    mime: "image/png".to_string(),
                    width: 512,
                    height: 512,
                    sticker_emoji: "\u{1F389}".to_string(),
                    is_sticker: true,
                    file: File {
                        relative_path: "chats/chat_2/stickers/sticker_1.png".to_string(),
                        size: png_sticker.len() as i64,
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
        // 10: GIF / animation
        Message {
            id: 19,
            date: t0 + 140,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6007,
                    date: t0 + 140,
                    mime: "video/mp4".to_string(),
                    width: 320,
                    height: 240,
                    duration: 3,
                    is_animated: true,
                    file: File {
                        relative_path: "chats/chat_2/animations/anim_1.mp4".to_string(),
                        size: 10,
                        ..Default::default()
                    },
                    thumb: Image {
                        width: 160,
                        height: 160,
                        file: File {
                            relative_path: "chats/chat_2/animations/anim_1.mp4_thumb.png"
                                .to_string(),
                            size: png_thumb.len() as i64,
                            ..Default::default()
                        },
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
        // 11: Shared Contact with vCard
        Message {
            id: 20,
            date: t0 + 150,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::SharedContact(SharedContact {
                    info: peer_dusk.info.clone(),
                    vcard: File {
                        content: b"BEGIN:VCARD\nFN:Dusk Vale\nEND:VCARD\n".to_vec(),
                        relative_path: "chats/chat_2/contacts/contact_1.vcard".to_string(),
                        size: 35,
                        ..Default::default()
                    },
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 12: Static location
        Message {
            id: 21,
            date: t0 + 160,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::GeoPoint(GeoPoint {
                    latitude: 47.3769,
                    longitude: 8.5417,
                    valid: true,
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 13: Live location
        Message {
            id: 22,
            date: t0 + 170,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::GeoPoint(GeoPoint {
                    latitude: 47.3770,
                    longitude: 8.5420,
                    valid: true,
                }),
                ttl: 900,
            },
            ..Default::default()
        },
        // 14: Venue
        Message {
            id: 23,
            date: t0 + 180,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            media: Media {
                content: MediaContent::Venue(Venue {
                    point: GeoPoint {
                        latitude: 47.3769,
                        longitude: 8.5417,
                        valid: true,
                    },
                    title: "Central Observatory".to_string(),
                    address: "100 Meridian Way".to_string(),
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 15: Poll
        Message {
            id: 24,
            date: t0 + 190,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            media: Media {
                content: MediaContent::Poll(Poll {
                    id: 7001,
                    question: vec![TextPart::plain("Preferred export format?")],
                    answers: vec![
                        PollAnswer {
                            text: vec![TextPart::plain("JSON")],
                            option: b"0".to_vec(),
                            votes: 3,
                            my: true,
                        },
                        PollAnswer {
                            text: vec![TextPart::plain("HTML")],
                            option: b"1".to_vec(),
                            votes: 1,
                            my: false,
                        },
                    ],
                    total_votes: 4,
                    closed: true,
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 16: TodoList
        Message {
            id: 25,
            date: t0 + 200,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            media: Media {
                content: MediaContent::TodoList(TodoList {
                    title: vec![TextPart::plain("Release Checklist")],
                    items: vec![
                        TodoListItem {
                            text: vec![TextPart::plain("Run golden tests")],
                            id: 1,
                        },
                        TodoListItem {
                            text: vec![TextPart::plain("Verify clippy")],
                            id: 2,
                        },
                    ],
                    others_can_append: true,
                    others_can_complete: true,
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 17: Game
        Message {
            id: 26,
            date: t0 + 210,
            from_id: bot_id,
            peer_id: briar_id,
            self_id,
            via_bot_id: UserId(1003),
            media: Media {
                content: MediaContent::Game(Game {
                    id: 8001,
                    bot_id: UserId(1003),
                    short_name: "star_dash".to_string(),
                    title: "Star Dash".to_string(),
                    description: "Arcade obstacle runner".to_string(),
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 18: Invoice
        Message {
            id: 27,
            date: t0 + 220,
            from_id: bot_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::Invoice(Invoice {
                    title: "Pro License".to_string(),
                    description: "Annual plan".to_string(),
                    currency: "USD".to_string(),
                    amount: 1999,
                    receipt_msg_id: 28,
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 19: PaidMedia
        Message {
            id: 28,
            date: t0 + 230,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::PaidMedia(PaidMedia {
                    stars: 250,
                    extended: Vec::new(),
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 20: GiveawayStart
        Message {
            id: 29,
            date: t0 + 240,
            from_id: chat_pub_chan.id(),
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::GiveawayStart(GiveawayStart {
                    channels: vec![ChannelId(3004)],
                    countries: vec!["US".to_string(), "DE".to_string()],
                    additional_prize: "Collector Badge".to_string(),
                    until_date: t0 + 86_400,
                    quantity: 3,
                    months: 6,
                    credits: 0,
                    all: true,
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 21: GiveawayResults
        Message {
            id: 30,
            date: t0 + 250,
            from_id: chat_pub_chan.id(),
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::GiveawayResults(GiveawayResults {
                    channel: ChannelId(3004),
                    winners: vec![briar_id, dusk_id],
                    additional_prize: "Collector Badge".to_string(),
                    until_date: t0 + 86_400,
                    launch_id: 29,
                    additional_peers_count: 0,
                    winners_count: 3,
                    unclaimed_count: 0,
                    months: 6,
                    credits: 0,
                    refunded: false,
                    all: true,
                }),
                ttl: 0,
            },
            ..Default::default()
        },
        // 22: UnsupportedMedia
        Message {
            id: 31,
            date: t0 + 260,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::Unsupported(UnsupportedMedia),
                ttl: 0,
            },
            ..Default::default()
        },
        // 23: RichMessage
        Message {
            id: 32,
            date: t0 + 270,
            from_id: self_id,
            peer_id: briar_id,
            self_id,
            out: true,
            rich_message: Some(rich_message),
            ..Default::default()
        },
        // 24: Forwarded from peer (in regular chat, not Saved Messages)
        Message {
            id: 33,
            date: t0 + 280,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            forwarded: true,
            forwarded_from_id: dusk_id,
            forwarded_date: t0 - 500,
            text: vec![TextPart::plain("Forwarded quote from Dusk")],
            ..Default::default()
        },
        // 25: Skipped file (FileSize)
        Message {
            id: 34,
            date: t0 + 290,
            from_id: briar_id,
            peer_id: briar_id,
            self_id,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 6099,
                    date: t0 + 290,
                    name: "huge.iso".to_string(),
                    mime: "application/octet-stream".to_string(),
                    file: File {
                        size: 99_000_000,
                        skip_reason: SkipReason::FileSize,
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                ttl: 0,
            },
            ..Default::default()
        },
    ];

    // Chat 3: Bot chat with service actions
    let bot_msgs = vec![
        Message {
            id: 1,
            date: t0 + 400,
            from_id: bot_id,
            peer_id: bot_id,
            self_id,
            action: ServiceAction::BotAllowed {
                app_id: 42,
                app: "Cinder Mini".to_string(),
                domain: "app.example.invalid".to_string(),
                attach_menu: false,
                from_request: false,
            },
            ..Default::default()
        },
        Message {
            id: 2,
            date: t0 + 410,
            from_id: self_id,
            peer_id: bot_id,
            self_id,
            out: true,
            action: ServiceAction::PaymentSent {
                currency: "USD".to_string(),
                amount: 1999,
                recurring_init: true,
                recurring_used: false,
            },
            ..Default::default()
        },
        Message {
            id: 3,
            date: t0 + 420,
            from_id: self_id,
            peer_id: bot_id,
            self_id,
            out: true,
            action: ServiceAction::SecureValuesSent {
                types: vec![SecureValueType::Passport, SecureValueType::Email],
            },
            ..Default::default()
        },
        Message {
            id: 4,
            date: t0 + 430,
            from_id: self_id,
            peer_id: bot_id,
            self_id,
            out: true,
            action: ServiceAction::GameScore {
                game_id: 8001,
                score: 9001,
            },
            reply_to_msg_id: 1,
            ..Default::default()
        },
    ];

    // Chat 4: Private group with rich set of service actions
    let group_msgs = vec![
        Message {
            id: 1,
            date: t0 + 500,
            from_id: self_id,
            peer_id: chat_priv_group.id(),
            self_id,
            out: true,
            action: ServiceAction::ChatCreate {
                title: "Lantern Squad".to_string(),
                user_ids: vec![UserId(1002), UserId(1004)],
            },
            ..Default::default()
        },
        Message {
            id: 2,
            date: t0 + 510,
            from_id: briar_id,
            peer_id: chat_priv_group.id(),
            self_id,
            action: ServiceAction::ChatEditTitle {
                title: "Lantern Squad v2".to_string(),
            },
            ..Default::default()
        },
        Message {
            id: 3,
            date: t0 + 520,
            from_id: briar_id,
            peer_id: chat_priv_group.id(),
            self_id,
            reply_to_msg_id: 1,
            action: ServiceAction::PinMessage,
            ..Default::default()
        },
        Message {
            id: 4,
            date: t0 + 530,
            from_id: self_id,
            peer_id: chat_priv_group.id(),
            self_id,
            out: true,
            action: ServiceAction::PhoneCall {
                conference_id: 0,
                state: PhoneCallState::MigrateConferenceCall,
                duration: 120,
            },
            ..Default::default()
        },
        Message {
            id: 5,
            date: t0 + 540,
            from_id: self_id,
            peer_id: chat_priv_group.id(),
            self_id,
            out: true,
            action: ServiceAction::GiftCredits {
                cost: "$4.99".to_string(),
                amount: CreditsAmount::new(10, 500_000_000, CreditsType::Ton),
            },
            ..Default::default()
        },
        Message {
            id: 6,
            date: t0 + 550,
            from_id: briar_id,
            peer_id: chat_priv_group.id(),
            self_id,
            action: ServiceAction::SuggestedPostApproval {
                reject_comment: String::new(),
                schedule_date: t0 + 3600,
                price: CreditsAmount::new(25, 0, CreditsType::Stars),
                rejected: false,
                balance_too_low: false,
            },
            ..Default::default()
        },
        Message {
            id: 7,
            date: t0 + 560,
            from_id: briar_id,
            peer_id: chat_priv_group.id(),
            self_id,
            action: ServiceAction::SuggestBirthday {
                birthday: Birthday::new(15, 6, 1998),
            },
            ..Default::default()
        },
        Message {
            id: 8,
            date: t0 + 570,
            from_id: self_id,
            peer_id: chat_priv_group.id(),
            self_id,
            out: true,
            action: ServiceAction::BoostApply { boosts: 2 },
            ..Default::default()
        },
        Message {
            id: 9,
            date: t0 + 580,
            from_id: self_id,
            peer_id: chat_priv_group.id(),
            self_id,
            out: true,
            action: ServiceAction::RequestedPeer {
                peers: vec![briar_id, dusk_id],
                button_id: 7,
            },
            ..Default::default()
        },
    ];

    // Remaining chat types (supergroups, channels, left channel)
    let simple_msg = |peer_id: PeerId, from_id: PeerId, text: &str, sig: &str, out: bool| Message {
        id: 1,
        date: t0 + 600,
        from_id,
        peer_id,
        self_id,
        out,
        signature: sig.to_string(),
        text: vec![TextPart::plain(text)],
        ..Default::default()
    };

    let mut dialog_slices = BTreeMap::new();
    dialog_slices.insert(
        self_id,
        vec![MessagesSlice {
            list: saved_msgs,
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        briar_id,
        vec![MessagesSlice {
            list: personal_msgs,
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        bot_id,
        vec![MessagesSlice {
            list: bot_msgs,
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        chat_priv_group.id(),
        vec![MessagesSlice {
            list: group_msgs,
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        chat_priv_super.id(),
        vec![MessagesSlice {
            list: vec![simple_msg(
                chat_priv_super.id(),
                briar_id,
                "Hello private supergroup",
                "",
                false,
            )],
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        chat_pub_super.id(),
        vec![MessagesSlice {
            list: vec![simple_msg(
                chat_pub_super.id(),
                self_id,
                "My public supergroup post",
                "",
                true,
            )],
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        chat_priv_chan.id(),
        vec![MessagesSlice {
            list: vec![simple_msg(
                chat_priv_chan.id(),
                chat_priv_chan.id(),
                "Private channel update",
                "Editor",
                false,
            )],
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        chat_pub_chan.id(),
        vec![MessagesSlice {
            list: vec![simple_msg(
                chat_pub_chan.id(),
                chat_pub_chan.id(),
                "Public broadcast",
                "Desk",
                false,
            )],
            peers: peers.clone(),
        }],
    );
    dialog_slices.insert(
        chat_left_chan.id(),
        vec![MessagesSlice {
            list: vec![simple_msg(
                chat_left_chan.id(),
                self_id,
                "Old outgoing message in left channel",
                "",
                true,
            )],
            peers: peers.clone(),
        }],
    );

    let mut source = InMemoryExportSource::default();
    source.personal_info = PersonalInfo {
        user: self_user.clone(),
        bio: "Fictional test account\nLine 2".to_string(),
    };
    source.userpic_slices = vec![UserpicsSlice {
        list: vec![Photo {
            id: 9001,
            date: t0,
            spoilered: false,
            image: Image {
                width: 600,
                height: 400,
                file: File {
                    relative_path: "profile_pictures/photo_1@14-11-2023_22-13-20.png".to_string(),
                    size: png_large.len() as i64,
                    ..Default::default()
                },
            },
        }],
    }];
    source.story_slices = vec![StoriesSlice {
        list: vec![
            Story {
                id: 1,
                date: t0 + 100,
                expires: t0 + 86_500,
                pinned: true,
                caption: vec![
                    TextPart::plain("Morning "),
                    TextPart::with_type(TextPartType::Hashtag, "#sky", ""),
                ],
                media: Media {
                    content: MediaContent::Photo(Photo {
                        id: 9101,
                        date: t0 + 100,
                        spoilered: false,
                        image: Image {
                            width: 600,
                            height: 400,
                            file: File {
                                relative_path: "stories/story_1@14-11-2023_22-15-00.png"
                                    .to_string(),
                                size: png_large.len() as i64,
                                ..Default::default()
                            },
                        },
                    }),
                    ttl: 0,
                },
            },
            Story {
                id: 2,
                date: t0 + 160,
                expires: t0 + 86_560,
                pinned: false,
                caption: vec![TextPart::plain("Clip")],
                media: Media {
                    content: MediaContent::Document(Box::new(Document {
                        id: 9102,
                        date: t0 + 160,
                        mime: "video/mp4".to_string(),
                        duration: 5,
                        is_video_file: true,
                        file: File {
                            relative_path: "stories/story_2@14-11-2023_22-16-00.mp4".to_string(),
                            size: 14,
                            ..Default::default()
                        },
                        thumb: Image {
                            width: 160,
                            height: 160,
                            file: File {
                                relative_path: "stories/story_2@14-11-2023_22-16-00.mp4_thumb.png"
                                    .to_string(),
                                size: png_thumb.len() as i64,
                                ..Default::default()
                            },
                        },
                        ..Default::default()
                    })),
                    ttl: 0,
                },
            },
        ],
        last_id: 2,
        skipped: 0,
    }];
    source.profile_music_slices = vec![ProfileMusicSlice {
        list: vec![Message {
            id: 1,
            date: t0 + 200,
            self_id,
            media: Media {
                content: MediaContent::Document(Box::new(Document {
                    id: 9201,
                    date: t0 + 200,
                    name: "audio_1.mp3".to_string(),
                    mime: "audio/mpeg".to_string(),
                    song_performer: "Echo Band".to_string(),
                    song_title: "Horizon".to_string(),
                    duration: 210,
                    is_audio_file: true,
                    file: File {
                        relative_path: "profile_music/audio_1.mp3".to_string(),
                        size: 13,
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
    source.contacts = ContactsList {
        list: vec![peer_dusk.info.clone(), peer_briar.info.clone()],
        correspondents: vec![TopPeer {
            peer: Peer::User(peer_briar.clone()),
            rating: 42.5,
        }],
        inline_bots: vec![TopPeer {
            peer: Peer::User(peer_bot.clone()),
            rating: 12.25,
        }],
        phone_calls: vec![TopPeer {
            peer: Peer::User(peer_dusk.clone()),
            rating: 5.0,
        }],
    };
    source.sessions = SessionsList {
        list: vec![Session {
            application_id: 2040,
            application_name: "Telegram Desktop".to_string(),
            application_version: "5.0.0".to_string(),
            device_model: "Workstation".to_string(),
            platform: "Windows".to_string(),
            system_version: "11".to_string(),
            ip: "198.51.100.10".to_string(),
            country: "Switzerland".to_string(),
            region: "Zurich".to_string(),
            created: t0 - 10_000,
            last_active: t0,
        }],
        web_list: vec![WebSession {
            bot_username: "cinder_test_bot".to_string(),
            domain: "portal.example.invalid".to_string(),
            browser: "Firefox".to_string(),
            platform: "Linux".to_string(),
            ip: "198.51.100.20".to_string(),
            region: "Berlin".to_string(),
            created: t0 - 5000,
            last_active: t0 - 100,
        }],
    };
    source.other_data_json = Some(b"{\"ip_history\":[\"198.51.100.1\"]}\n".to_vec());
    source.dialogs = dialogs;
    source.dialog_messages = dialog_slices;

    settings.path = dir_with_slash(export_root);
    (source, settings, environment)
}

#[tokio::test]
async fn golden_full_account_json_and_html_export() {
    let dir = temp_export_dir("full_account");
    let (source, settings, env) = build_comprehensive_source(&dir);

    let (mut controller, _handle) = ExportController::new(source, InputPeer::Empty);
    controller.set_skip_path_normalization(true);
    let final_state = controller.run(&settings, &env).await;
    match final_state {
        ExportState::Finished(fin) => {
            assert!(
                fin.path.ends_with("export_results.html"),
                "unexpected main path: {}",
                fin.path
            );
            assert!(fin.files_count > 44);
            assert!(fin.bytes_count > 0);
        }
        other => panic!("expected Finished state, got: {other:?}"),
    }

    // 1. Verify all 44 static HTML assets were copied byte-for-byte.
    assert_eq!(EXPORT_HTML_ASSETS.len(), 44);
    for asset in EXPORT_HTML_ASSETS {
        let on_disk = fs::read(dir.join(asset.relative_path))
            .unwrap_or_else(|e| panic!("missing static asset {}: {e}", asset.relative_path));
        assert_eq!(
            on_disk.as_slice(),
            asset.bytes,
            "asset mismatch for {}",
            asset.relative_path
        );
    }

    // 2. Parse and verify `result.json` against C++ `export_output_json.cpp` structure.
    let json_raw = fs::read_to_string(dir.join("result.json")).expect("read result.json");
    let root: Value = serde_json::from_str(&json_raw).expect("result.json must be valid JSON");

    // Personal info
    assert_eq!(root["personal_information"]["user_id"], 1001);
    assert_eq!(root["personal_information"]["first_name"], "Alden");
    assert_eq!(root["personal_information"]["last_name"], "Vance");
    assert_eq!(
        root["personal_information"]["phone_number"],
        "+1 555 010 1001"
    );
    assert_eq!(root["personal_information"]["username"], "@alden_v");
    assert_eq!(
        root["personal_information"]["bio"],
        "Fictional test account\nLine 2"
    );

    // Profile pictures, stories, profile music, contacts, frequent contacts, sessions, other data
    assert_eq!(root["profile_pictures"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["profile_pictures"][0]["photo"],
        "profile_pictures/photo_1@14-11-2023_22-13-20.png"
    );
    assert_eq!(root["stories"].as_array().unwrap().len(), 2);
    assert_eq!(root["stories"][0]["pinned"], true);
    assert_eq!(
        root["stories"][0]["media"],
        "stories/story_1@14-11-2023_22-15-00.png"
    );
    assert_eq!(
        root["stories"][1]["media"],
        "stories/story_2@14-11-2023_22-16-00.mp4"
    );
    assert_eq!(root["profile_music"].as_array().unwrap().len(), 1);
    assert_eq!(root["profile_music"][0]["performer"], "Echo Band");
    assert_eq!(root["profile_music"][0]["title"], "Horizon");

    assert_eq!(root["contacts"]["list"].as_array().unwrap().len(), 2);
    assert_eq!(
        root["frequent_contacts"]["list"].as_array().unwrap().len(),
        3
    );
    assert_eq!(root["frequent_contacts"]["list"][0]["category"], "people");
    assert_eq!(
        root["frequent_contacts"]["list"][1]["category"],
        "inline_bots"
    );
    assert_eq!(root["frequent_contacts"]["list"][2]["category"], "calls");

    assert_eq!(root["sessions"]["list"].as_array().unwrap().len(), 1);
    assert_eq!(root["web_sessions"]["list"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["web_sessions"]["list"][0]["bot_username"],
        "cinder_test_bot"
    );
    assert_eq!(root["other_data"]["ip_history"][0], "198.51.100.1");

    // Chats list: 8 active + 1 left channel
    let chats = root["chats"]["list"].as_array().unwrap();
    assert_eq!(chats.len(), 8);
    let types: Vec<&str> = chats.iter().map(|c| c["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        vec![
            "saved_messages",
            "personal_chat",
            "bot_chat",
            "private_group",
            "private_supergroup",
            "public_supergroup",
            "private_channel",
            "public_channel",
        ]
    );
    let left_chats = root["left_chats"]["list"].as_array().unwrap();
    assert_eq!(left_chats.len(), 1);
    assert_eq!(left_chats[0]["name"], "Archived Beacon");

    // Saved messages check
    let saved_m0 = &chats[0]["messages"][0];
    assert_eq!(saved_m0["forwarded_from"], "Briar Moss");
    assert_eq!(saved_m0["forwarded_from_id"], "user1002");
    assert_eq!(saved_m0["saved_from"], "Briar Moss");
    let saved_m1 = &chats[0]["messages"][1];
    assert_eq!(saved_m1["forwarded_from"], "Hidden Sender <Anon>");

    // Personal chat messages: verify every media kind, text entity, reaction, inline button, rich message
    let pmsgs = chats[1]["messages"].as_array().unwrap();
    assert_eq!(pmsgs.len(), 25);

    // Message 10: all entities + edited + reactions + inline_bot_buttons
    let m10 = &pmsgs[0];
    assert_eq!(m10["id"], 10);
    assert_eq!(m10["date"], "2023-11-14T22:13:20");
    assert_eq!(m10["date_unixtime"], "1700000000");
    assert_eq!(m10["edited"], "2023-11-14T22:14:05");
    assert_eq!(m10["edited_unixtime"], "1700000045");
    let entities = m10["text_entities"].as_array().unwrap();
    let entity_types: Vec<&str> = entities
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    for expected in [
        "plain",
        "unknown",
        "mention",
        "hashtag",
        "bot_command",
        "link",
        "email",
        "bold",
        "italic",
        "code",
        "pre",
        "text_link",
        "mention_name",
        "phone",
        "cashtag",
        "underline",
        "strikethrough",
        "blockquote",
        "bank_card",
        "ton_address",
        "spoiler",
        "custom_emoji",
    ] {
        assert!(
            entity_types.contains(&expected),
            "missing JSON entity type {expected} in {entity_types:?}"
        );
    }
    assert_eq!(m10["reactions"].as_array().unwrap().len(), 3);
    assert_eq!(m10["reactions"][0]["type"], "emoji");
    assert_eq!(m10["reactions"][0]["count"], 2);
    assert_eq!(m10["reactions"][1]["type"], "custom_emoji");
    assert_eq!(
        m10["reactions"][1]["document_id"],
        "stickers/custom_emoji_1.png"
    );
    assert_eq!(m10["reactions"][2]["type"], "paid");
    assert_eq!(m10["inline_bot_buttons"][0][0]["type"], "url");
    assert_eq!(m10["inline_bot_buttons"][0][1]["type"], "callback");

    // Verify media fields on messages 11..=34
    assert_eq!(pmsgs[1]["reply_to_message_id"], 10);
    assert_eq!(pmsgs[1]["media_spoiler"], true);
    assert_eq!(pmsgs[1]["photo"], "chats/chat_2/photos/photo_1.png");
    assert_eq!(pmsgs[2]["reply_to_peer_id"], "chat2001");
    assert_eq!(pmsgs[2]["self_destruct_period_seconds"], 10);
    assert_eq!(pmsgs[3]["file"], "chats/chat_2/files/report.pdf");
    assert_eq!(pmsgs[4]["media_type"], "audio_file");
    assert_eq!(pmsgs[4]["performer"], "Synth Trio");
    assert_eq!(pmsgs[4]["title"], "Night Signal");
    assert_eq!(pmsgs[5]["media_type"], "voice_message");
    assert_eq!(pmsgs[6]["media_type"], "video_file");
    assert_eq!(pmsgs[7]["media_type"], "video_message");
    assert_eq!(pmsgs[8]["media_type"], "sticker");
    assert_eq!(pmsgs[9]["media_type"], "animation");
    assert_eq!(pmsgs[10]["contact_information"]["first_name"], "Dusk");
    assert_eq!(
        pmsgs[10]["contact_vcard"],
        "chats/chat_2/contacts/contact_1.vcard"
    );
    assert_eq!(pmsgs[11]["location_information"]["latitude"], 47.3769);
    assert_eq!(pmsgs[12]["live_location_period_seconds"], 900);
    assert_eq!(pmsgs[13]["place_name"], "Central Observatory");
    assert_eq!(pmsgs[14]["poll"]["closed"], true);
    assert_eq!(pmsgs[14]["poll"]["total_voters"], 4);
    assert_eq!(pmsgs[14]["poll"]["answers"][0]["chosen"], true);
    assert_eq!(pmsgs[15]["todo_list"]["title"], "Release Checklist");
    assert_eq!(pmsgs[16]["game_title"], "Star Dash");
    assert_eq!(
        pmsgs[16]["game_link"],
        "https://t.me/cinder_test_bot?game=star_dash"
    );
    // Matches C++ `export_output_json.cpp:2209` where invoice_information is pushed as JSON string
    assert!(pmsgs[17]["invoice_information"].is_string());
    assert_eq!(pmsgs[18]["paid_stars_amount"], 250);
    assert_eq!(pmsgs[19]["giveaway_information"]["quantity"], 3);
    assert_eq!(pmsgs[20]["giveaway_results"]["winners_count"], 3);
    assert_eq!(pmsgs[21]["type"], "unsupported");
    assert_eq!(pmsgs[22]["rich_message"]["rtl"], false);
    assert_eq!(pmsgs[22]["rich_message"]["part"], false);
    assert_eq!(
        pmsgs[22]["rich_message"]["blocks"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(pmsgs[23]["forwarded_from"], "Dusk Vale");
    assert_eq!(
        pmsgs[24]["file"],
        "(File exceeds maximum size. Change data exporting settings to download.)"
    );

    // Group service actions check (including upstream quirks preserved verbatim)
    let gmsgs = chats[3]["messages"].as_array().unwrap();
    assert_eq!(gmsgs[0]["action"], "create_group");
    assert_eq!(gmsgs[1]["action"], "edit_group_title");
    assert_eq!(gmsgs[2]["action"], "pin_message");
    assert_eq!(gmsgs[3]["discard_reason"], "migrate_conference_all");
    assert_eq!(gmsgs[4]["action"], "send_ton_gift");
    assert_eq!(gmsgs[5]["price_amount_whole"], "25");
    assert_eq!(gmsgs[5]["price_currency"], "Stars");
    assert_eq!(gmsgs[6]["action"], "suggest_birthday");
    assert_eq!(gmsgs[7]["action"], "boost_apply");
    assert!(gmsgs[8]["peers"].is_string());

    // 3. Verify HTML files against C++ `export_output_html.cpp` markup.
    let summary_html =
        fs::read_to_string(dir.join("export_results.html")).expect("read export_results.html");
    assert!(summary_html.starts_with(
        "<!DOCTYPE html>\n<html>\n\n <head>\n\n  <meta charset=\"utf-8\"/>\n<title>Exported Data</title>"
    ));
    assert!(summary_html.contains("\n       <div class=\"value bold\">\nAlden\n       </div>\n"));
    assert!(summary_html.contains("+1 555 010 1001"));
    assert!(summary_html.contains("@alden_v"));
    assert!(summary_html.contains("Fictional test account<br>Line 2"));
    assert!(summary_html.contains("lists/chats.html"));
    assert!(summary_html.contains("lists/contacts.html"));
    assert!(summary_html.contains("lists/frequent.html"));
    assert!(summary_html.contains("lists/profile_pictures.html"));
    assert!(summary_html.contains("lists/stories.html"));
    assert!(summary_html.contains("lists/profile_music.html"));
    assert!(summary_html.contains("lists/sessions.html"));
    assert!(summary_html.contains("lists/web_sessions.html"));
    assert!(summary_html.contains("other_data.json"));
    // Verify thumbnail generated for personal info avatar
    assert!(
        summary_html.contains("_info.jpg") || summary_html.contains("profile_pictures/photo_1")
    );

    for list_file in [
        "lists/chats.html",
        "lists/contacts.html",
        "lists/frequent.html",
        "lists/profile_pictures.html",
        "lists/stories.html",
        "lists/profile_music.html",
        "lists/sessions.html",
        "lists/web_sessions.html",
    ] {
        let html = fs::read_to_string(dir.join(list_file))
            .unwrap_or_else(|e| panic!("missing {list_file}: {e}"));
        assert!(html.starts_with("<!DOCTYPE html>\n<html>"));
        assert!(html.trim_end().ends_with("</html>"));
    }

    let chats_html = fs::read_to_string(dir.join("lists/chats.html")).unwrap();
    assert!(chats_html.contains("Saved messages"));
    assert!(chats_html.contains("Briar Moss"));
    assert!(chats_html.contains("Cinder Bot"));
    assert!(chats_html.contains("Lantern Squad"));
    assert!(chats_html.contains("Archived Beacon"));

    let chat2_html = fs::read_to_string(dir.join("chats/chat_2/messages.html")).unwrap();
    assert!(chat2_html.contains("14 November 2023"));
    assert!(chat2_html.contains("Plain &amp; &lt;&quot;&apos;&gt; "));
    assert!(chat2_html.contains("<strong>bold</strong>"));
    assert!(chat2_html.contains("<em>italic</em>"));
    assert!(chat2_html.contains("<code>let x = 1;</code>"));
    assert!(chat2_html.contains("<pre>fn main() {}<br></pre>"));
    assert!(chat2_html.contains("<u>under</u>"));
    assert!(chat2_html.contains("<s>strike</s>"));
    assert!(chat2_html.contains("<blockquote>quoted line</blockquote>"));
    assert!(chat2_html.contains("<span class=\"spoiler hidden\" onclick=\"ShowSpoiler(this)\">"));
    assert!(chat2_html.contains("ShowNotLoadedEmoji()"));
    assert!(chat2_html.contains("ShowNotAvailableEmoji()"));
    assert!(chat2_html.contains("media_poll"));
    assert!(chat2_html.contains("total details&#x09;"));
    assert!(chat2_html.contains("media_giveaway"));
    assert!(chat2_html.contains("media_invoice"));
    assert!(chat2_html.contains("media_venue"));
    assert!(chat2_html.contains("media_live_location"));
    assert!(chat2_html.contains("media_game"));
    assert!(chat2_html.contains("bot_buttons_table"));
    assert!(chat2_html.contains("rich_collage_box"));
    assert!(chat2_html.contains("This message is not supported by this version of Rustgram."));
    assert!(chat2_html.trim_end().ends_with("</html>"));

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn golden_html_pagination_every_1000_messages() {
    let dir = temp_export_dir("pagination");
    let self_user = build_user(
        1001,
        "Alden",
        "Vance",
        "alden_v",
        "15550101001",
        false,
        true,
    );
    let peer_briar = build_user(
        1002,
        "Briar",
        "Moss",
        "briar_m",
        "15550101002",
        false,
        false,
    );
    let self_id = self_user.id();
    let briar_id = peer_briar.id();

    let mut peers = BTreeMap::new();
    peers.insert(self_id, Peer::User(self_user));
    peers.insert(briar_id, Peer::User(peer_briar.clone()));

    let total_msgs = MESSAGES_IN_FILE * 2 + 5; // 2005 messages -> 3 pages: 1000, 1000, 5
    let base_date = 1_700_000_000_i32;
    let mut messages = Vec::with_capacity(total_msgs);
    for i in 1..=total_msgs {
        let id = i as i32;
        let reply_to_msg_id = if id == 1005 {
            10 // reply across page boundary to messages.html
        } else if id == 1006 {
            1005 // reply within messages2.html
        } else {
            0
        };
        messages.push(Message {
            id,
            date: base_date + id,
            from_id: if id % 2 == 0 { briar_id } else { self_id },
            peer_id: briar_id,
            self_id,
            out: id % 2 != 0,
            reply_to_msg_id,
            text: vec![TextPart::plain(format!("Msg #{id}"))],
            ..Default::default()
        });
    }

    let settings = Settings {
        path: dir_with_slash(&dir),
        format: OutputFormat::Html,
        types: ExportTypes::PERSONAL_CHATS,
        full_chats: ExportTypes::PERSONAL_CHATS | ExportTypes::BOT_CHATS,
        single_peer: InputPeer::User(InputPeerUser {
            user_id: 1002,
            access_hash: 1,
        }),
        ..Default::default()
    };

    let mut dialogs = DialogsInfo {
        chats: vec![DialogInfo {
            kind: DialogType::Personal,
            name: peer_briar.info.first_name.clone(),
            last_name: peer_briar.info.last_name.clone(),
            peer_id: briar_id,
            color_index: peer_briar.color_index,
            ..Default::default()
        }],
        left: Vec::new(),
    };
    finalize_dialogs_info(&mut dialogs, &settings);

    let mut dialog_slices = BTreeMap::new();
    dialog_slices.insert(
        briar_id,
        vec![MessagesSlice {
            list: messages,
            peers,
        }],
    );

    let mut source = InMemoryExportSource::default();
    source.dialogs = dialogs;
    source.dialog_messages = dialog_slices;

    let (mut controller, _handle) = ExportController::new(source, settings.single_peer.clone());
    controller.set_skip_path_normalization(true);
    let state = controller.run(&settings, &Environment::default()).await;
    assert!(matches!(state, ExportState::Finished(_)), "got {state:?}");

    let p1 = fs::read_to_string(dir.join("messages.html")).expect("messages.html");
    let p2 = fs::read_to_string(dir.join("messages2.html")).expect("messages2.html");
    let p3 = fs::read_to_string(dir.join("messages3.html")).expect("messages3.html");
    assert!(!dir.join("messages4.html").exists());

    // Page 1: no Previous, has Next -> messages2.html, contains Msg #1 and Msg #1000, not Msg #1001
    // Inside html > body > page_wrap > page_body > history, block indentation is 5 spaces.
    assert!(!p1.contains("Previous messages"));
    assert!(p1.contains(
        "\n     <a class=\"pagination block_link\" href=\"messages2.html\">\nNext messages\n     </a>\n"
    ));
    assert!(p1.contains("Msg #1\n"));
    assert!(p1.contains("Msg #1000\n"));
    assert!(!p1.contains("Msg #1001\n"));

    // Page 2: Previous -> messages.html, Next -> messages3.html, cross-page & same-page reply links
    assert!(p2.contains(
        "\n     <a class=\"pagination block_link\" href=\"messages.html\">\nPrevious messages\n     </a>\n"
    ));
    assert!(p2.contains(
        "\n     <a class=\"pagination block_link\" href=\"messages3.html\">\nNext messages\n     </a>\n"
    ));
    assert!(p2.contains("Msg #1001\n"));
    assert!(p2.contains("Msg #2000\n"));
    assert!(!p2.contains("Msg #2001\n"));
    assert!(
        p2.contains("<a href=\"messages.html#go_to_message10\">this message</a>"),
        "expected cross-page reply link in messages2.html"
    );
    assert!(
        p2.contains(
            "<a href=\"#go_to_message1005\" onclick=\"return GoToMessage(1005)\">this message</a>"
        ),
        "expected same-page reply link in messages2.html"
    );

    // Page 3: Previous -> messages2.html, no Next, contains Msg #2001..2005
    assert!(p3.contains(
        "\n     <a class=\"pagination block_link\" href=\"messages2.html\">\nPrevious messages\n     </a>\n"
    ));
    assert!(!p3.contains("Next messages"));
    assert!(p3.contains("Msg #2001\n"));
    assert!(p3.contains("Msg #2005\n"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn golden_escaping_json_and_html() {
    // JSON escaping matches `export_output_json.cpp:SerializeString`
    let raw = "a\"b\\c\nd\re\tf\x00\x08\x1F\u{2028}\u{2029}Привет\u{1F680}";
    let json_escaped = String::from_utf8(serialize_json_string(raw)).unwrap();
    assert_eq!(
        json_escaped,
        "\"a\\\"b\\\\c\\nd\\re\\tf\\u0000\\u0008\\u001F\\u2028\\u2029Привет\u{1F680}\""
    );
    let parsed: Value = serde_json::from_str(&json_escaped).expect("valid JSON string");
    assert_eq!(parsed.as_str().unwrap(), raw);

    // HTML escaping matches `export_output_html.cpp:SerializeString`
    let html_raw = "<tag attr=\"1\" & '2'>\nline2\r\t\x01\x1F\u{2028}\u{2029}日本語\u{1F31F}";
    let html_escaped = serialize_html_string(html_raw);
    assert_eq!(
        html_escaped,
        "&lt;tag attr=&quot;1&quot; &amp; &apos;2&apos;&gt;<br>line2&#x0D;&#x09;&#x01;&#x1F;<br><br>日本語\u{1F31F}"
    );
}

#[test]
fn golden_file_naming_collision_and_path_normalization() {
    let dir = temp_export_dir("filenames");
    let base = dir_with_slash(&dir);

    // Multi-dot collision numbering: archive.tar.gz -> archive (1).tar.gz -> archive (2).tar.gz
    let first = OutputFile::prepare_relative_path(&base, "files/archive.tar.gz");
    assert_eq!(first, "files/archive.tar.gz");
    write_fixture_file(&dir, &first, b"1");

    let second = OutputFile::prepare_relative_path(&base, "files/archive.tar.gz");
    assert_eq!(second, "files/archive (1).tar.gz");
    write_fixture_file(&dir, &second, b"2");

    let third = OutputFile::prepare_relative_path(&base, "files/archive.tar.gz");
    assert_eq!(third, "files/archive (2).tar.gz");

    // Sanitization & Windows reserved name rules (`base::FileNameFromUserString`)
    assert_eq!(
        file_name_from_user_string("a/b\\c:d*e?f\"g<h>i|j"),
        "a_b_c_d_e_f_g_h_i_j"
    );
    assert_eq!(file_name_from_user_string("trailing."), "trailing._");
    assert_eq!(file_name_from_user_string("trailing "), "trailing _");
    assert_eq!(file_name_from_user_string("CON"), "_CON");
    assert_eq!(file_name_from_user_string("nul.txt"), "_nul.txt");
    assert_eq!(file_name_from_user_string("COM1.log"), "_COM1.log");
    assert_eq!(
        file_name_from_user_string("shortcut.lnk"),
        "shortcut.lnk.download"
    );
    assert_eq!(file_name_from_user_string("desk.SCF"), "desk.SCF.download");
    assert_eq!(
        file_name_from_user_string("hi\u{202E}gpj.exe"),
        "hi_gpj.exe"
    );

    // NormalizePath behavior: empty dir stays, non-empty dir creates ChatExport_YYYY-MM-DD,
    // repeated calls disambiguate with (1), (2).
    let empty_dir = dir.join("empty_target");
    fs::create_dir_all(&empty_dir).unwrap();
    let norm_empty = normalize_path_with_date(
        &Settings {
            path: empty_dir.to_string_lossy().into_owned(),
            force_sub_path: false,
            ..Default::default()
        },
        "2023-11-14",
    );
    assert_eq!(norm_empty, dir_with_slash(&empty_dir));

    write_fixture_file(&empty_dir, "existing.txt", b"hi");
    // Full export -> DataExport_YYYY-MM-DD/
    let norm_sub1 = normalize_path_with_date(
        &Settings {
            path: empty_dir.to_string_lossy().into_owned(),
            force_sub_path: false,
            ..Default::default()
        },
        "2023-11-14",
    );
    assert!(
        norm_sub1.ends_with("DataExport_2023-11-14/"),
        "got {norm_sub1}"
    );
    fs::create_dir_all(&norm_sub1).unwrap();

    let norm_sub2 = normalize_path_with_date(
        &Settings {
            path: empty_dir.to_string_lossy().into_owned(),
            force_sub_path: false,
            ..Default::default()
        },
        "2023-11-14",
    );
    assert!(
        norm_sub2.ends_with("DataExport_2023-11-14 (1)/"),
        "got {norm_sub2}"
    );

    // Single-peer export -> ChatExport_YYYY-MM-DD/
    let norm_chat1 = normalize_path_with_date(
        &Settings {
            path: empty_dir.to_string_lossy().into_owned(),
            single_peer: InputPeer::Self_,
            force_sub_path: false,
            ..Default::default()
        },
        "2023-11-14",
    );
    assert!(
        norm_chat1.ends_with("ChatExport_2023-11-14/"),
        "got {norm_chat1}"
    );
    fs::create_dir_all(&norm_chat1).unwrap();

    let norm_chat2 = normalize_path_with_date(
        &Settings {
            path: empty_dir.to_string_lossy().into_owned(),
            single_peer: InputPeer::Self_,
            force_sub_path: false,
            ..Default::default()
        },
        "2023-11-14",
    );
    assert!(
        norm_chat2.ends_with("ChatExport_2023-11-14 (1)/"),
        "got {norm_chat2}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn settings_qt_roundtrip_and_validation() {
    for peer in [
        InputPeer::Empty,
        InputPeer::Self_,
        InputPeer::User(InputPeerUser {
            user_id: 123_456,
            access_hash: 987_654_321,
        }),
        InputPeer::Chat(InputPeerChat { chat_id: 55_555 }),
        InputPeer::Channel(InputPeerChannel {
            channel_id: 999_888,
            access_hash: -42,
        }),
    ] {
        let s = Settings {
            path: "/exports/test/".to_string(),
            format: OutputFormat::HtmlAndJson,
            types: ExportTypes::PERSONAL_INFO | ExportTypes::PERSONAL_CHATS,
            full_chats: ExportTypes::PERSONAL_CHATS | ExportTypes::BOT_CHATS,
            media: MediaSettings {
                types: MediaTypes::PHOTO | MediaTypes::VIDEO,
                size_limit: 4 * 1024 * 1024,
            },
            available_at: 1_700_000_123,
            single_peer: peer.clone(),
            single_peer_from: 1_700_000_000,
            single_peer_till: 1_700_100_000,
            ..Default::default()
        };
        assert!(s.validate());
        let bytes = s.serialize_qt();
        let decoded = Settings::deserialize_qt(&bytes).expect("roundtrip decode");
        assert_eq!(decoded.path, s.path);
        assert_eq!(decoded.format, s.format);
        assert_eq!(decoded.types, s.types);
        assert_eq!(decoded.full_chats, s.full_chats);
        assert_eq!(decoded.media, s.media);
        assert_eq!(decoded.available_at, s.available_at);
        assert_eq!(decoded.single_peer, peer);
        assert_eq!(decoded.single_peer_from, s.single_peer_from);
        assert_eq!(decoded.single_peer_till, s.single_peer_till);
    }

    // Invalid date range (`till <= from`) fails validation.
    let bad = Settings {
        single_peer_from: 200,
        single_peer_till: 100,
        ..Default::default()
    };
    assert!(!bad.validate());
}
