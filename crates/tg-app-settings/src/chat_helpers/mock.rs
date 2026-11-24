//! Mock implementation of [`ChatHelpersBackend`] with synthetic animated Lottie stickers,
//! procedural static stickers, animated GIF mosaic items, emoji keywords, and autocomplete data.
//!
//! Uses only synthetic fictional names and original procedural artwork/Lottie JSON.

use std::collections::{BTreeSet, HashMap};

use tg_chat::autocomplete::{RecentHashtags, RecentInlineBots};
use tg_chat::emoji::{
    EmojiKeywordDiff, EmojiKeywordResult, EmojiKeywords, EmojiVariants, LegacyEmojiCompleter,
    LegacyReplacement, RecentEmojiId, RecentEmojiStore, SkinTone, strip_skin_tone,
};
use tg_chat::stickers::{
    EmojiSearchOptions, PanelBuildOptions, StickerDocument, StickerPanelSection, StickerSetUpdate,
    StickerTabIcon, StickerType, StickersSetFlags, StickersStore, StickersType,
};
use tg_chat::{
    BotCommand, DocumentId, LastseenStatus, Size, StickerSetId, TimeId, UserId, UserInfo,
};
use tg_text::emoji as tg_emoji_db;

use crate::chat_helpers::backend::{
    ChatHelpersBackend, GifCategory, GifItem, InlineGifQueryHook, StickerMedia,
};

const SET_LOTTIE_CRITTERS: StickerSetId = StickerSetId(10_001);
const SET_GEOMETRIC_BOTS: StickerSetId = StickerSetId(10_002);
const SET_COSMIC_PREMIUM: StickerSetId = StickerSetId(10_003);
const SET_TRENDING_SHAPES: StickerSetId = StickerSetId(10_004);

/// Default mock inline-bot GIF query hook (`@gif`).
#[derive(Debug, Clone)]
pub struct MockInlineGifHook {
    catalog: Vec<GifItem>,
}

impl MockInlineGifHook {
    /// Creates a mock catalog of procedural animated GIFs with varied aspect ratios.
    #[must_use]
    pub fn new() -> Self {
        let catalog = vec![
            GifItem {
                id: DocumentId(50_001),
                title: "Neon Wave".into(),
                width: 180,
                height: 112,
                tags: vec![
                    "wave".into(),
                    "hi".into(),
                    "hello".into(),
                    "neon".into(),
                    "👋".into(),
                ],
                saved: true,
                frame_count: 16,
                fps: 12.0,
            },
            GifItem {
                id: DocumentId(50_002),
                title: "Party Confetti".into(),
                width: 132,
                height: 132,
                tags: vec![
                    "party".into(),
                    "tada".into(),
                    "celebrate".into(),
                    "🎉".into(),
                    "😂".into(),
                ],
                saved: true,
                frame_count: 16,
                fps: 14.0,
            },
            GifItem {
                id: DocumentId(50_003),
                title: "Pulse Heart".into(),
                width: 148,
                height: 118,
                tags: vec!["heart".into(), "love".into(), "❤️".into(), "cute".into()],
                saved: true,
                frame_count: 16,
                fps: 12.0,
            },
            GifItem {
                id: DocumentId(50_004),
                title: "Campfire Glow".into(),
                width: 120,
                height: 140,
                tags: vec!["fire".into(), "hot".into(), "lit".into(), "🔥".into()],
                saved: true,
                frame_count: 16,
                fps: 14.0,
            },
            GifItem {
                id: DocumentId(50_005),
                title: "Orbiting Stars".into(),
                width: 196,
                height: 112,
                tags: vec![
                    "star".into(),
                    "space".into(),
                    "rocket".into(),
                    "🚀".into(),
                    "⭐".into(),
                ],
                saved: true,
                frame_count: 16,
                fps: 12.0,
            },
            GifItem {
                id: DocumentId(50_006),
                title: "Thumbs Up Bounce".into(),
                width: 140,
                height: 120,
                tags: vec![
                    "ok".into(),
                    "yes".into(),
                    "thumbs".into(),
                    "👍".into(),
                    "nice".into(),
                ],
                saved: true,
                frame_count: 16,
                fps: 12.0,
            },
            GifItem {
                id: DocumentId(50_007),
                title: "Matrix Rain".into(),
                width: 168,
                height: 110,
                tags: vec![
                    "code".into(),
                    "rust".into(),
                    "hack".into(),
                    "dev".into(),
                    "🔥".into(),
                ],
                saved: false,
                frame_count: 16,
                fps: 14.0,
            },
            GifItem {
                id: DocumentId(50_008),
                title: "Sunset Ripple".into(),
                width: 136,
                height: 136,
                tags: vec![
                    "chill".into(),
                    "wave".into(),
                    "sun".into(),
                    "😂".into(),
                    "❤️".into(),
                ],
                saved: false,
                frame_count: 16,
                fps: 10.0,
            },
            GifItem {
                id: DocumentId(50_009),
                title: "Disco Grid".into(),
                width: 176,
                height: 116,
                tags: vec![
                    "dance".into(),
                    "party".into(),
                    "music".into(),
                    "🎉".into(),
                    "👍".into(),
                ],
                saved: false,
                frame_count: 16,
                fps: 14.0,
            },
        ];
        Self { catalog }
    }
}

impl Default for MockInlineGifHook {
    fn default() -> Self {
        Self::new()
    }
}

impl InlineGifQueryHook for MockInlineGifHook {
    fn query_inline_gifs(&self, _bot_username: &str, query: &str) -> Vec<GifItem> {
        let q = query.trim().to_lowercase();
        if q.is_empty() || q == "trending" {
            return self.catalog.clone();
        }
        let q_base = strip_skin_tone(&q);
        self.catalog
            .iter()
            .filter(|item| {
                item.title.to_lowercase().contains(&q)
                    || item.tags.iter().any(|t| {
                        let tl = t.to_lowercase();
                        tl.contains(&q) || (!q_base.is_empty() && strip_skin_tone(&tl) == q_base)
                    })
            })
            .cloned()
            .collect()
    }
}

/// Complete offline mock backend for `chat_helpers` UI, screenshots, and unit tests.
pub struct MockChatHelpersBackend {
    recent_emoji: RecentEmojiStore,
    emoji_variants: EmojiVariants,
    emoji_keywords: EmojiKeywords,
    stickers: StickersStore,
    sticker_media: HashMap<DocumentId, StickerMedia>,
    is_premium: bool,
    now: TimeId,
    gif_hook: Box<dyn InlineGifQueryHook + Send + Sync>,
    saved_gif_ids: Vec<DocumentId>,
    gif_catalog: HashMap<DocumentId, GifItem>,
    self_user_id: UserId,
    users: Vec<UserInfo>,
    last_authors: Vec<UserId>,
    bot_commands: HashMap<UserId, Vec<BotCommand>>,
    recent_inline_bots: RecentInlineBots,
    recent_hashtags: RecentHashtags,
}

impl Default for MockChatHelpersBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockChatHelpersBackend {
    /// Builds a richly populated mock backend with animated Lottie stickers, static packs,
    /// premium-locked stickers, GIFs, emoji keywords, and chat autocomplete participants.
    #[must_use]
    pub fn new() -> Self {
        let now = 1_730_000_000;

        // 1. Emoji keywords seeded from `tg-text`'s complete shortcode list plus English keywords.
        let mut legacy = LegacyEmojiCompleter::new();
        for entry in tg_emoji_db::all_suggestions() {
            legacy.add(LegacyReplacement::new(entry.emoji, entry.replacement));
        }
        let mut emoji_keywords = EmojiKeywords::with_legacy(legacy);
        emoji_keywords.set_remote_languages(vec!["en".into()]);
        emoji_keywords.apply_difference(
            "en",
            1,
            &[
                EmojiKeywordDiff::Added {
                    keyword: "smile".into(),
                    emoticons: vec![
                        "😀".into(),
                        "😃".into(),
                        "😄".into(),
                        "😊".into(),
                        "😁".into(),
                        "🙂".into(),
                    ],
                },
                EmojiKeywordDiff::Added {
                    keyword: "happy".into(),
                    emoticons: vec!["😊".into(), "😄".into(), "🥳".into(), "😁".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "love".into(),
                    emoticons: vec!["❤️".into(), "😍".into(), "🥰".into(), "💖".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "heart".into(),
                    emoticons: vec![
                        "❤️".into(),
                        "🧡".into(),
                        "💛".into(),
                        "💚".into(),
                        "💙".into(),
                    ],
                },
                EmojiKeywordDiff::Added {
                    keyword: "fire".into(),
                    emoticons: vec!["🔥".into(), "💥".into(), "✨".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "rocket".into(),
                    emoticons: vec!["🚀".into(), "🛸".into(), "🌟".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "thumbs".into(),
                    emoticons: vec!["👍".into(), "👎".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "wave".into(),
                    emoticons: vec!["👋".into(), "🌊".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "party".into(),
                    emoticons: vec!["🎉".into(), "🥳".into(), "🎊".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "cat".into(),
                    emoticons: vec!["🐱".into(), "😺".into(), "😸".into(), "😻".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "dog".into(),
                    emoticons: vec!["🐶".into(), "🐕".into(), "🐾".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "coffee".into(),
                    emoticons: vec!["☕".into(), "🍵".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "star".into(),
                    emoticons: vec!["⭐".into(), "🌟".into(), "✨".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "ok".into(),
                    emoticons: vec!["👌".into(), "👍".into(), "✅".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "hi".into(),
                    emoticons: vec!["👋".into(), "😊".into()],
                },
            ],
        );

        let mut recent_emoji = RecentEmojiStore::new();
        recent_emoji.resolve();

        // 2. Sticker sets & media.
        let mut stickers = StickersStore::new();
        let mut sticker_media = HashMap::new();

        // Pack 1: "Animated Shapes" (Lottie JSON rendered live by `tg-lottie`)
        let lottie_defs: &[(u64, &str, &str)] = &[
            (
                1001,
                "⭐",
                &make_lottie_star([1.0, 0.78, 0.16], [1.0, 0.56, 0.08]),
            ),
            (
                1002,
                "❤️",
                &make_lottie_pulse_circle([0.95, 0.25, 0.38], [1.0, 0.55, 0.65]),
            ),
            (
                1003,
                "🚀",
                &make_lottie_orbit([0.22, 0.64, 0.96], [0.98, 0.84, 0.22]),
            ),
            (
                1004,
                "🔥",
                &make_lottie_spinner([0.98, 0.44, 0.14], [1.0, 0.76, 0.18]),
            ),
            (
                1005,
                "🎉",
                &make_lottie_star([0.56, 0.36, 0.96], [0.28, 0.84, 0.76]),
            ),
            (
                1006,
                "😄",
                &make_lottie_smile([1.0, 0.82, 0.22], [0.22, 0.16, 0.10]),
            ),
            (
                1007,
                "🌊",
                &make_lottie_orbit([0.16, 0.74, 0.88], [0.58, 0.92, 0.98]),
            ),
            (
                1008,
                "✨",
                &make_lottie_spinner([0.32, 0.82, 0.48], [0.76, 0.96, 0.40]),
            ),
            (
                1009,
                "💜",
                &make_lottie_pulse_circle([0.62, 0.34, 0.94], [0.86, 0.62, 1.0]),
            ),
            (
                1010,
                "👍",
                &make_lottie_smile([0.36, 0.76, 0.98], [0.10, 0.24, 0.40]),
            ),
        ];
        let mut pack1_docs = Vec::new();
        let mut pack1_emoji: Vec<(String, Vec<DocumentId>)> = Vec::new();
        for &(id, alt, json) in lottie_defs {
            let doc_id = DocumentId(id);
            pack1_docs.push(doc_id);
            pack1_emoji.push((alt.to_string(), vec![doc_id]));
            stickers.insert_document(StickerDocument {
                id: doc_id,
                set_id: Some(SET_LOTTIE_CRITTERS),
                sticker_type: StickerType::Tgs,
                alt_emoji: alt.to_string(),
                dimensions: Size::new(512, 512),
                thumbnail_size: Some(Size::new(128, 128)),
                is_premium_sticker: false,
                is_premium_emoji: false,
                is_gifv: false,
            });
            sticker_media.insert(doc_id, StickerMedia::LottieJson(json.to_string()));
        }

        // Pack 2: "Geometric Badges" (static RGBA illustrations)
        type StaticStickerDef = (u64, &'static str, [u8; 3], [u8; 3], u8);
        let static_defs: &[StaticStickerDef] = &[
            (2001, "😄", [0x40, 0xA7, 0xE3], [0x21, 0x76, 0xC7], 0),
            (2002, "🔥", [0xFF, 0x7B, 0x54], [0xE0, 0x3E, 0x3E], 1),
            (2003, "❤️", [0xF0, 0x5A, 0x94], [0xC2, 0x29, 0x67], 2),
            (2004, "🚀", [0x58, 0xB9, 0x70], [0x2B, 0x8A, 0x4E], 3),
            (2005, "🎉", [0x9B, 0x6B, 0xFF], [0x6A, 0x3A, 0xD4], 4),
            (2006, "👍", [0xF7, 0xB7, 0x31], [0xE6, 0x7E, 0x22], 0),
            (2007, "⭐", [0x26, 0xC6, 0xDA], [0x00, 0x83, 0x8F], 1),
            (2008, "✨", [0xAB, 0x47, 0xBC], [0x6A, 0x1B, 0x9A], 2),
        ];
        let mut pack2_docs = Vec::new();
        let mut pack2_emoji: Vec<(String, Vec<DocumentId>)> = Vec::new();
        for &(id, alt, c1, c2, shape) in static_defs {
            let doc_id = DocumentId(id);
            pack2_docs.push(doc_id);
            pack2_emoji.push((alt.to_string(), vec![doc_id]));
            stickers.insert_document(StickerDocument {
                id: doc_id,
                set_id: Some(SET_GEOMETRIC_BOTS),
                sticker_type: StickerType::Webp,
                alt_emoji: alt.to_string(),
                dimensions: Size::new(512, 512),
                thumbnail_size: Some(Size::new(128, 128)),
                is_premium_sticker: false,
                is_premium_emoji: false,
                is_gifv: false,
            });
            let rgba = render_static_sticker_rgba(96, c1, c2, shape);
            sticker_media.insert(
                doc_id,
                StickerMedia::StaticRgba {
                    width: 96,
                    height: 96,
                    rgba,
                },
            );
        }

        // Pack 3: "Cosmic Gems" (Premium-locked animated Lottie stickers!)
        let premium_defs: &[(u64, &str, &str)] = &[
            (
                3001,
                "💎",
                &make_lottie_spinner([0.46, 0.36, 0.98], [0.95, 0.42, 0.86]),
            ),
            (
                3002,
                "⭐",
                &make_lottie_star([0.98, 0.72, 0.18], [0.95, 0.32, 0.62]),
            ),
            (
                3003,
                "🔥",
                &make_lottie_pulse_circle([0.96, 0.34, 0.68], [0.52, 0.38, 0.98]),
            ),
            (
                3004,
                "🚀",
                &make_lottie_orbit([0.60, 0.40, 0.98], [0.24, 0.86, 0.96]),
            ),
            (
                3005,
                "✨",
                &make_lottie_star([0.24, 0.82, 0.96], [0.68, 0.42, 0.98]),
            ),
        ];
        let mut pack3_docs = Vec::new();
        let mut pack3_emoji: Vec<(String, Vec<DocumentId>)> = Vec::new();
        for &(id, alt, json) in premium_defs {
            let doc_id = DocumentId(id);
            pack3_docs.push(doc_id);
            pack3_emoji.push((alt.to_string(), vec![doc_id]));
            stickers.insert_document(StickerDocument {
                id: doc_id,
                set_id: Some(SET_COSMIC_PREMIUM),
                sticker_type: StickerType::Tgs,
                alt_emoji: alt.to_string(),
                dimensions: Size::new(512, 512),
                thumbnail_size: Some(Size::new(128, 128)),
                is_premium_sticker: true,
                is_premium_emoji: false,
                is_gifv: false,
            });
            sticker_media.insert(doc_id, StickerMedia::LottieJson(json.to_string()));
        }

        // Pack 4: Trending/Featured pack (not yet installed)
        let trending_defs: &[StaticStickerDef] = &[
            (4001, "🐱", [0xFF, 0x9F, 0x43], [0xEE, 0x52, 0x53], 3),
            (4002, "🐶", [0x1D, 0xD1, 0xA1], [0x10, 0xAC, 0x84], 4),
            (4003, "🎉", [0x54, 0xA0, 0xFF], [0x2E, 0x86, 0xDE], 1),
            (4004, "❤️", [0xFF, 0x6B, 0x6B], [0xEE, 0x52, 0x53], 2),
            (4005, "⭐", [0xFE, 0xCA, 0x57], [0xFF, 0x9F, 0x43], 0),
        ];
        let mut pack4_docs = Vec::new();
        let mut pack4_emoji: Vec<(String, Vec<DocumentId>)> = Vec::new();
        for &(id, alt, c1, c2, shape) in trending_defs {
            let doc_id = DocumentId(id);
            pack4_docs.push(doc_id);
            pack4_emoji.push((alt.to_string(), vec![doc_id]));
            stickers.insert_document(StickerDocument {
                id: doc_id,
                set_id: Some(SET_TRENDING_SHAPES),
                sticker_type: StickerType::Webp,
                alt_emoji: alt.to_string(),
                dimensions: Size::new(512, 512),
                thumbnail_size: Some(Size::new(128, 128)),
                is_premium_sticker: false,
                is_premium_emoji: false,
                is_gifv: false,
            });
            let rgba = render_static_sticker_rgba(96, c1, c2, shape);
            sticker_media.insert(
                doc_id,
                StickerMedia::StaticRgba {
                    width: 96,
                    height: 96,
                    rgba,
                },
            );
        }

        // Feed installed sets & featured set into `StickersStore`.
        stickers.sets_received(
            StickersType::Stickers,
            vec![
                StickerSetUpdate {
                    id: SET_LOTTIE_CRITTERS,
                    access_hash: 111,
                    hash: 1,
                    title: "Animated Shapes".into(),
                    short_name: "animated_shapes".into(),
                    count: pack1_docs.len() as i32,
                    flags: StickersSetFlags::INSTALLED | StickersSetFlags::OFFICIAL,
                    install_date: Some(now - 1000),
                    thumbnail_document_id: Some(DocumentId(1001)),
                },
                StickerSetUpdate {
                    id: SET_GEOMETRIC_BOTS,
                    access_hash: 222,
                    hash: 2,
                    title: "Geometric Badges".into(),
                    short_name: "geometric_badges".into(),
                    count: pack2_docs.len() as i32,
                    flags: StickersSetFlags::INSTALLED,
                    install_date: Some(now - 2000),
                    thumbnail_document_id: Some(DocumentId(2001)),
                },
                StickerSetUpdate {
                    id: SET_COSMIC_PREMIUM,
                    access_hash: 333,
                    hash: 3,
                    title: "Cosmic Gems (Premium)".into(),
                    short_name: "cosmic_gems".into(),
                    count: pack3_docs.len() as i32,
                    flags: StickersSetFlags::INSTALLED | StickersSetFlags::OFFICIAL,
                    install_date: Some(now - 3000),
                    thumbnail_document_id: Some(DocumentId(3001)),
                },
            ],
            now,
        );
        stickers.feed_set_stickers(SET_LOTTIE_CRITTERS, pack1_docs, &pack1_emoji);
        stickers.feed_set_stickers(SET_GEOMETRIC_BOTS, pack2_docs, &pack2_emoji);
        stickers.feed_set_stickers(SET_COSMIC_PREMIUM, pack3_docs, &pack3_emoji);

        let mut unread = BTreeSet::new();
        unread.insert(SET_TRENDING_SHAPES);
        stickers.featured_sets_received(
            StickersType::Stickers,
            vec![StickerSetUpdate {
                id: SET_TRENDING_SHAPES,
                access_hash: 444,
                hash: 4,
                title: "Trending Emblems".into(),
                short_name: "trending_emblems".into(),
                count: pack4_docs.len() as i32,
                flags: StickersSetFlags::FEATURED | StickersSetFlags::OFFICIAL,
                install_date: None,
                thumbnail_document_id: Some(DocumentId(4001)),
            }],
            &unread,
        );
        stickers.feed_set_stickers(SET_TRENDING_SHAPES, pack4_docs, &pack4_emoji);

        // Seed Favorites (including one Premium-locked sticker so lock badges are immediately visible)
        // and Recent stickers.
        stickers.set_faved(DocumentId(3001), true, None, false);
        stickers.set_faved(DocumentId(1001), true, None, false);
        stickers.set_faved(DocumentId(1002), true, None, false);
        stickers.set_faved(DocumentId(2001), true, None, false);

        stickers.increment_sticker(DocumentId(1006), now - 50);
        stickers.increment_sticker(DocumentId(1003), now - 40);
        stickers.increment_sticker(DocumentId(2002), now - 30);
        stickers.increment_sticker(DocumentId(1004), now - 20);
        stickers.increment_sticker(DocumentId(1005), now - 10);

        // 3. GIFs hook & saved GIFs.
        let hook = MockInlineGifHook::new();
        let all_gifs = hook.query_inline_gifs("gif", "");
        let saved_gif_ids = all_gifs.iter().filter(|g| g.saved).map(|g| g.id).collect();
        let gif_catalog = all_gifs.into_iter().map(|g| (g.id, g)).collect();

        // 4. Autocomplete users (purely fictional handles, no real people's names).
        let self_user_id = UserId(1);
        let users = vec![
            make_user(
                10,
                "Kestrel",
                "kestrel_dev",
                LastseenStatus::online_till_status(now + 300, false, false),
                false,
                "",
            ),
            make_user(
                11,
                "Bramble",
                "bramble_ui",
                LastseenStatus::online_till_status(now + 120, false, false),
                false,
                "",
            ),
            make_user(
                12,
                "Cedar",
                "cedar_ops",
                LastseenStatus::recently(false),
                false,
                "",
            ),
            make_user(13, "Linden", "", LastseenStatus::recently(false), false, ""),
            make_user(
                14,
                "Rowan",
                "rowan_qa",
                LastseenStatus::within_week(false),
                false,
                "",
            ),
            make_user(
                20,
                "Nova Bot",
                "novabot",
                LastseenStatus::long_ago(false),
                true,
                "Search docs...",
            ),
            make_user(
                21,
                "GIF Bot",
                "gif",
                LastseenStatus::long_ago(false),
                true,
                "Search GIFs...",
            ),
        ];
        let last_authors = vec![UserId(11), UserId(10), UserId(20)];
        let mut recent_inline_bots = RecentInlineBots::new();
        recent_inline_bots.bump(UserId(20));
        recent_inline_bots.bump(UserId(21));

        let mut bot_commands = HashMap::new();
        bot_commands.insert(
            UserId(20),
            vec![
                BotCommand {
                    command: "start".into(),
                    description: "Start a session with Nova Bot".into(),
                },
                BotCommand {
                    command: "help".into(),
                    description: "Show available commands and tips".into(),
                },
                BotCommand {
                    command: "poll".into(),
                    description: "Create a quick group poll".into(),
                },
                BotCommand {
                    command: "remind".into(),
                    description: "Schedule a private reminder".into(),
                },
                BotCommand {
                    command: "status".into(),
                    description: "Check workspace build status".into(),
                },
            ],
        );

        let mut recent_hashtags = RecentHashtags::new();
        for tag in ["release", "design", "stickers", "rustgram", "rust"] {
            recent_hashtags.increment(tag);
        }
        recent_hashtags.increment("rust");

        Self {
            recent_emoji,
            emoji_variants: EmojiVariants::new(),
            emoji_keywords,
            stickers,
            sticker_media,
            is_premium: false,
            now,
            gif_hook: Box::new(hook),
            saved_gif_ids,
            gif_catalog,
            self_user_id,
            users,
            last_authors,
            bot_commands,
            recent_inline_bots,
            recent_hashtags,
        }
    }

    /// Replaces the inline-bot GIF query hook (e.g. for custom providers or tests).
    pub fn set_inline_gif_hook(&mut self, hook: Box<dyn InlineGifQueryHook + Send + Sync>) {
        self.gif_hook = hook;
    }
}

impl ChatHelpersBackend for MockChatHelpersBackend {
    fn recent_emoji(&mut self) -> Vec<String> {
        self.recent_emoji
            .visible_recent(false, true)
            .into_iter()
            .filter_map(|id| match id {
                RecentEmojiId::Unicode(s) => Some(s),
                RecentEmojiId::Document(_) => None,
            })
            .collect()
    }

    fn record_emoji_used(&mut self, emoji: &str) {
        self.recent_emoji
            .increment(RecentEmojiId::Unicode(emoji.to_string()));
    }

    fn hide_recent_emoji(&mut self, emoji: &str) {
        self.recent_emoji
            .hide(&RecentEmojiId::Unicode(emoji.to_string()));
    }

    fn emoji_variants(&self) -> &EmojiVariants {
        &self.emoji_variants
    }

    fn save_emoji_variant(&mut self, base_emoji: &str, tone: SkinTone) {
        self.emoji_variants.save_variant(base_emoji, tone);
    }

    fn save_all_emoji_variants(&mut self, tone: SkinTone) {
        self.emoji_variants.save_all_variants(tone);
    }

    fn search_emoji(&mut self, query: &str) -> Vec<EmojiKeywordResult> {
        let recent = self.recent_emoji.list().to_vec();
        self.emoji_keywords
            .query_mine(query, false, &recent, &self.emoji_variants, |e| {
                tg_emoji_db::find_emoji(e, 0).is_some_and(|(er, _)| er.has_variants())
            })
    }

    fn is_premium(&self) -> bool {
        self.is_premium
    }

    fn set_premium(&mut self, premium: bool) {
        self.is_premium = premium;
    }

    fn sticker_sections(&self) -> Vec<StickerPanelSection> {
        // Always include premium stickers in sections so locked badges are displayed to non-premium users.
        self.stickers.build_my_sections(PanelBuildOptions {
            is_masks: false,
            premium_possible: true,
            unlimited_recent: false,
            exclude_set_id: None,
            megagroup_set: None,
        })
    }

    fn featured_sticker_sections(&self) -> Vec<StickerPanelSection> {
        self.stickers.build_featured_sections(PanelBuildOptions {
            is_masks: false,
            premium_possible: true,
            unlimited_recent: false,
            exclude_set_id: None,
            megagroup_set: None,
        })
    }

    fn sticker_tab_icons(
        &self,
        sections: &[StickerPanelSection],
        icon_side: i32,
    ) -> Vec<StickerTabIcon> {
        self.stickers.build_tab_icons(sections, icon_side)
    }

    fn sticker_document(&self, id: DocumentId) -> Option<&StickerDocument> {
        self.stickers.documents.get(&id)
    }

    fn sticker_media(&self, id: DocumentId) -> Option<StickerMedia> {
        self.sticker_media.get(&id).cloned()
    }

    fn search_stickers(&self, query: &str) -> Vec<DocumentId> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        // First try direct emoji lookup (or keyword -> emojis -> stickers).
        let mut emojis: Vec<String> = Vec::new();
        if let Some((er, _)) = tg_emoji_db::find_emoji(q, 0) {
            emojis.push(er.non_colored_id().to_string());
        } else {
            for hit in self.emoji_keywords.query(q, false).into_iter().take(6) {
                emojis.push(strip_skin_tone(&hit.emoji));
            }
        }
        let refs: Vec<&str> = emojis.iter().map(String::as_str).collect();
        let mut results = self.stickers.get_list_by_emoji(
            &refs,
            None,
            EmojiSearchOptions {
                seed: 42,
                is_premium: self.is_premium,
                normals_per_premium: 2,
                premiums_to_end_for_free: 2,
            },
        );

        // Also match installed/featured set titles.
        let ql = q.to_lowercase();
        for set in self.stickers.sets.values() {
            if set.title.to_lowercase().contains(&ql) || set.short_name.to_lowercase().contains(&ql)
            {
                for &doc_id in &set.stickers {
                    if !results.contains(&doc_id) {
                        results.push(doc_id);
                    }
                }
            }
        }
        results
    }

    fn record_sticker_used(&mut self, id: DocumentId) {
        self.now += 1;
        self.stickers.increment_sticker(id, self.now);
    }

    fn is_sticker_faved(&self, id: DocumentId) -> bool {
        self.stickers.is_faved(id)
    }

    fn toggle_faved_sticker(&mut self, id: DocumentId) -> bool {
        let next = !self.stickers.is_faved(id);
        self.stickers.set_faved(id, next, None, self.is_premium);
        next
    }

    fn install_sticker_set(&mut self, set_id: StickerSetId) -> bool {
        self.now += 1;
        self.stickers.install_locally(set_id, self.now)
    }

    fn remove_sticker_set(&mut self, set_id: StickerSetId) -> bool {
        self.stickers.undo_install_locally(set_id)
    }

    fn is_set_installed(&self, set_id: StickerSetId) -> bool {
        self.stickers
            .sets
            .get(&set_id)
            .is_some_and(tg_chat::stickers::StickersSet::is_in_my_list)
    }

    fn saved_gifs(&self) -> Vec<GifItem> {
        self.saved_gif_ids
            .iter()
            .filter_map(|id| {
                let mut item = self.gif_catalog.get(id)?.clone();
                item.saved = true;
                Some(item)
            })
            .collect()
    }

    fn gif_categories(&self) -> Vec<GifCategory> {
        vec![
            GifCategory {
                id: "recent".into(),
                emoji: String::new(),
                label: "Saved GIFs".into(),
                query: String::new(),
            },
            GifCategory {
                id: "trending".into(),
                emoji: "🔥".into(),
                label: "Trending".into(),
                query: "trending".into(),
            },
            GifCategory {
                id: "joy".into(),
                emoji: "😂".into(),
                label: "Funny".into(),
                query: "😂".into(),
            },
            GifCategory {
                id: "love".into(),
                emoji: "❤️".into(),
                label: "Love".into(),
                query: "❤️".into(),
            },
            GifCategory {
                id: "thumbs".into(),
                emoji: "👍".into(),
                label: "Agree".into(),
                query: "👍".into(),
            },
            GifCategory {
                id: "party".into(),
                emoji: "🎉".into(),
                label: "Celebrate".into(),
                query: "🎉".into(),
            },
        ]
    }

    fn search_gifs(&self, query: &str) -> Vec<GifItem> {
        self.gif_hook
            .query_inline_gifs("gif", query)
            .into_iter()
            .map(|mut item| {
                item.saved = self.saved_gif_ids.contains(&item.id);
                item
            })
            .collect()
    }

    fn toggle_saved_gif(&mut self, id: DocumentId) -> bool {
        if let Some(pos) = self.saved_gif_ids.iter().position(|&x| x == id) {
            self.saved_gif_ids.remove(pos);
            false
        } else {
            self.saved_gif_ids.insert(0, id);
            true
        }
    }

    fn gif_frame_rgba(&self, id: DocumentId, frame_index: usize) -> Option<(u32, u32, Vec<u8>)> {
        let item = self.gif_catalog.get(&id)?;
        let w = item.width.clamp(64, 220);
        let h = item.height.clamp(64, 160);
        let phase =
            (frame_index % item.frame_count.max(1)) as f32 / (item.frame_count.max(1) as f32);
        let rgba = render_procedural_gif_frame(id.0, w, h, phase);
        Some((w, h, rgba))
    }

    fn self_user_id(&self) -> UserId {
        self.self_user_id
    }

    fn autocomplete_users(&self) -> &[UserInfo] {
        &self.users
    }

    fn autocomplete_last_authors(&self) -> &[UserId] {
        &self.last_authors
    }

    fn autocomplete_bot_commands(&self) -> &HashMap<UserId, Vec<BotCommand>> {
        &self.bot_commands
    }

    fn recent_inline_bot_ids(&self) -> &[UserId] {
        self.recent_inline_bots.list()
    }

    fn remove_recent_inline_bot(&mut self, id: UserId) -> bool {
        self.recent_inline_bots.remove(id)
    }

    fn recent_hashtags(&self) -> &RecentHashtags {
        &self.recent_hashtags
    }

    fn record_hashtag(&mut self, tag: &str) {
        self.recent_hashtags.increment(tag);
    }

    fn remove_hashtag(&mut self, tag: &str) -> bool {
        self.recent_hashtags.remove(tag)
    }

    fn search_emoji_suggestions(&mut self, keyword: &str, exact: bool) -> Vec<EmojiKeywordResult> {
        let recent = self.recent_emoji.list().to_vec();
        self.emoji_keywords
            .query_mine(keyword, exact, &recent, &self.emoji_variants, |e| {
                tg_emoji_db::find_emoji(e, 0).is_some_and(|(er, _)| er.has_variants())
            })
    }

    fn stickers_for_emoji(&self, emoji: &str) -> Vec<DocumentId> {
        self.stickers.get_list_by_emoji(
            &[emoji],
            None,
            EmojiSearchOptions {
                seed: 7,
                is_premium: self.is_premium,
                normals_per_premium: 2,
                premiums_to_end_for_free: 2,
            },
        )
    }
}

fn make_user(
    id: u64,
    name: &str,
    username: &str,
    lastseen: LastseenStatus,
    is_bot: bool,
    inline_placeholder: &str,
) -> UserInfo {
    let name_words = name.split_whitespace().map(|w| w.to_lowercase()).collect();
    UserInfo {
        id: UserId(id),
        access_hash: id * 1_009 + 17,
        first_name: name.to_string(),
        display_name: name.to_string(),
        username: username.to_string(),
        usernames: if username.is_empty() {
            Vec::new()
        } else {
            vec![username.to_string()]
        },
        name_words,
        is_bot,
        lastseen,
        inline_placeholder: inline_placeholder.to_string(),
        ..UserInfo::default()
    }
}

// --- Original hand-authored Lottie JSON generators (parsed & rasterized by `tg-lottie`) ---

fn make_lottie_star(fill: [f32; 3], stroke: [f32; 3]) -> String {
    format!(
        r#"{{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 60,
  "w": 128,
  "h": 128,
  "nm": "AnimatedStar",
  "layers": [
    {{
      "ty": 4,
      "nm": "Star",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 1, "k": [
          {{ "t": 0, "s": [-12], "e": [12] }},
          {{ "t": 30, "s": [12], "e": [-12] }},
          {{ "t": 60, "s": [-12] }}
        ] }},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 1, "k": [
          {{ "t": 0, "s": [90, 90, 100], "e": [110, 110, 100] }},
          {{ "t": 30, "s": [110, 110, 100], "e": [90, 90, 100] }},
          {{ "t": 60, "s": [90, 90, 100] }}
        ] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "nm": "StarGroup",
          "it": [
            {{
              "ty": "sr",
              "sy": 1,
              "pt": {{ "a": 0, "k": 5 }},
              "p": {{ "a": 0, "k": [0, 0] }},
              "r": {{ "a": 0, "k": 0 }},
              "ir": {{ "a": 0, "k": 22 }},
              "or": {{ "a": 0, "k": 48 }},
              "is": {{ "a": 0, "k": 0 }},
              "os": {{ "a": 0, "k": 0 }}
            }},
            {{
              "ty": "fl",
              "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }},
              "o": {{ "a": 0, "k": 100 }}
            }},
            {{
              "ty": "st",
              "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }},
              "o": {{ "a": 0, "k": 100 }},
              "w": {{ "a": 0, "k": 5 }},
              "lj": 2
            }},
            {{
              "ty": "tr",
              "p": {{ "a": 0, "k": [0, 0] }},
              "a": {{ "a": 0, "k": [0, 0] }},
              "s": {{ "a": 0, "k": [100, 100] }},
              "r": {{ "a": 0, "k": 0 }},
              "o": {{ "a": 0, "k": 100 }}
            }}
          ]
        }}
      ]
    }}
  ]
}}"#,
        fill[0], fill[1], fill[2], stroke[0], stroke[1], stroke[2]
    )
}

fn make_lottie_pulse_circle(outer: [f32; 3], inner: [f32; 3]) -> String {
    format!(
        r#"{{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 60,
  "w": 128,
  "h": 128,
  "nm": "PulseBadge",
  "layers": [
    {{
      "ty": 4,
      "nm": "Inner",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 0, "k": 0 }},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 1, "k": [
          {{ "t": 0, "s": [78, 78, 100], "e": [108, 108, 100] }},
          {{ "t": 20, "s": [108, 108, 100], "e": [86, 86, 100] }},
          {{ "t": 40, "s": [86, 86, 100], "e": [104, 104, 100] }},
          {{ "t": 60, "s": [78, 78, 100] }}
        ] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "el", "p": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [52, 52] }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }}
      ]
    }},
    {{
      "ty": 4,
      "nm": "Outer",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 0, "k": 0 }},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 1, "k": [
          {{ "t": 0, "s": [90, 90, 100], "e": [106, 106, 100] }},
          {{ "t": 30, "s": [106, 106, 100], "e": [90, 90, 100] }},
          {{ "t": 60, "s": [90, 90, 100] }}
        ] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "el", "p": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [92, 92] }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }}
      ]
    }}
  ]
}}"#,
        inner[0], inner[1], inner[2], outer[0], outer[1], outer[2]
    )
}

fn make_lottie_orbit(planet: [f32; 3], moon: [f32; 3]) -> String {
    format!(
        r#"{{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 60,
  "w": 128,
  "h": 128,
  "nm": "Orbit",
  "layers": [
    {{
      "ty": 4,
      "nm": "Moon",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 1, "k": [
          {{ "t": 0, "s": [0], "e": [360] }},
          {{ "t": 60, "s": [360] }}
        ] }},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 0, "k": [100, 100, 100] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "el", "p": {{ "a": 0, "k": [42, 0] }}, "s": {{ "a": 0, "k": [20, 20] }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }}
      ]
    }},
    {{
      "ty": 4,
      "nm": "Planet",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 0, "k": 0 }},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 0, "k": [100, 100, 100] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "el", "p": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [60, 60] }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }},
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "el", "p": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [86, 86] }} }},
            {{ "ty": "st", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 45 }}, "w": {{ "a": 0, "k": 3 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }}
      ]
    }}
  ]
}}"#,
        moon[0], moon[1], moon[2], planet[0], planet[1], planet[2], planet[0], planet[1], planet[2]
    )
}

fn make_lottie_spinner(primary: [f32; 3], accent: [f32; 3]) -> String {
    format!(
        r#"{{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 60,
  "w": 128,
  "h": 128,
  "nm": "Prism",
  "layers": [
    {{
      "ty": 4,
      "nm": "InnerDiamond",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 1, "k": [
          {{ "t": 0, "s": [45], "e": [-135] }},
          {{ "t": 60, "s": [-135] }}
        ] }},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 0, "k": [100, 100, 100] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "rc", "p": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [46, 46] }}, "r": {{ "a": 0, "k": 10 }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }}
      ]
    }},
    {{
      "ty": 4,
      "nm": "OuterSquare",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 1, "k": [
          {{ "t": 0, "s": [0], "e": [180] }},
          {{ "t": 60, "s": [180] }}
        ] }},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 0, "k": [100, 100, 100] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "rc", "p": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [78, 78] }}, "r": {{ "a": 0, "k": 18 }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }}
      ]
    }}
  ]
}}"#,
        accent[0], accent[1], accent[2], primary[0], primary[1], primary[2]
    )
}

fn make_lottie_smile(face: [f32; 3], ink: [f32; 3]) -> String {
    format!(
        r#"{{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 60,
  "w": 128,
  "h": 128,
  "nm": "BobbingSmile",
  "layers": [
    {{
      "ty": 4,
      "nm": "Face",
      "ip": 0,
      "op": 60,
      "st": 0,
      "ks": {{
        "o": {{ "a": 0, "k": 100 }},
        "r": {{ "a": 1, "k": [
          {{ "t": 0, "s": [-8], "e": [8] }},
          {{ "t": 30, "s": [8], "e": [-8] }},
          {{ "t": 60, "s": [-8] }}
        ] }},
        "p": {{ "a": 1, "k": [
          {{ "t": 0, "s": [64, 68, 0], "e": [64, 58, 0] }},
          {{ "t": 30, "s": [64, 58, 0], "e": [64, 68, 0] }},
          {{ "t": 60, "s": [64, 68, 0] }}
        ] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 0, "k": [100, 100, 100] }}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "el", "p": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [88, 88] }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }},
        {{
          "ty": "gr",
          "it": [
            {{ "ty": "el", "p": {{ "a": 0, "k": [-16, -10] }}, "s": {{ "a": 0, "k": [11, 13] }} }},
            {{ "ty": "el", "p": {{ "a": 0, "k": [16, -10] }}, "s": {{ "a": 0, "k": [11, 13] }} }},
            {{ "ty": "rc", "p": {{ "a": 0, "k": [0, 15] }}, "s": {{ "a": 0, "k": [36, 12] }}, "r": {{ "a": 0, "k": 6 }} }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.3}, {:.3}, {:.3}, 1] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "tr", "p": {{ "a": 0, "k": [0, 0] }}, "a": {{ "a": 0, "k": [0, 0] }}, "s": {{ "a": 0, "k": [100, 100] }}, "r": {{ "a": 0, "k": 0 }}, "o": {{ "a": 0, "k": 100 }} }}
          ]
        }}
      ]
    }}
  ]
}}"#,
        face[0], face[1], face[2], ink[0], ink[1], ink[2]
    )
}

// --- Procedural static sticker & animated GIF frame rasterizers ---

fn render_static_sticker_rgba(size: u32, top_rgb: [u8; 3], bot_rgb: [u8; 3], shape: u8) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let half = size as f32 * 0.5;
    let rad = half * 0.84;

    for y in 0..size {
        let fy = (y as f32 + 0.5 - half) / rad;
        let t = (y as f32 / (size - 1) as f32).clamp(0.0, 1.0);
        let r = lerp_u8(top_rgb[0], bot_rgb[0], t);
        let g = lerp_u8(top_rgb[1], bot_rgb[1], t);
        let b = lerp_u8(top_rgb[2], bot_rgb[2], t);

        for x in 0..size {
            let fx = (x as f32 + 0.5 - half) / rad;
            let dist = match shape % 5 {
                0 => (fx * fx + fy * fy).sqrt(),
                1 => fx.abs().max(fy.abs()) * 1.12,
                2 => (fx.abs() + fy.abs()) * 0.92,
                3 => ((fx * fx).powf(1.5) + (fy * fy).powf(1.5)).powf(1.0 / 3.0),
                _ => {
                    let ang = fy.atan2(fx);
                    let r_len = (fx * fx + fy * fy).sqrt();
                    r_len / (0.82 + 0.18 * (ang * 5.0).cos())
                }
            };
            let aa = ((1.0 - dist) * rad * 0.5 + 0.5).clamp(0.0, 1.0);
            if aa <= 0.0 {
                continue;
            }
            // Cute inner geometric eyes & highlight dot.
            let eye_l = ((fx + 0.28).powi(2) + (fy + 0.14).powi(2)).sqrt();
            let eye_r = ((fx - 0.28).powi(2) + (fy + 0.14).powi(2)).sqrt();
            let smile = ((fx * 0.9).powi(2) + (fy - 0.18).powi(2)).sqrt();
            let is_feature =
                eye_l < 0.11 || eye_r < 0.11 || (smile > 0.22 && smile < 0.31 && fy > 0.22);

            let (pr, pg, pb) = if is_feature {
                (255u8, 255u8, 255u8)
            } else {
                // Subtle top gloss.
                let gloss = ((1.0 - ((fx + 0.25).powi(2) + (fy + 0.48).powi(2)).sqrt()) * 0.28)
                    .clamp(0.0, 0.28);
                (
                    lerp_u8(r, 255, gloss),
                    lerp_u8(g, 255, gloss),
                    lerp_u8(b, 255, gloss),
                )
            };
            let idx = ((y * size + x) * 4) as usize;
            out[idx] = pr;
            out[idx + 1] = pg;
            out[idx + 2] = pb;
            out[idx + 3] = (aa * 255.0).round() as u8;
        }
    }
    out
}

fn render_procedural_gif_frame(seed: u64, w: u32, h: u32, phase: f32) -> Vec<u8> {
    let mut out = vec![255u8; (w * h * 4) as usize];
    let palettes: [([u8; 3], [u8; 3], [u8; 3]); 6] = [
        ([0x1A, 0x2A, 0x6C], [0xB2, 0x1F, 0x1F], [0xFD, 0xBB, 0x2D]),
        ([0x0F, 0x20, 0x27], [0x20, 0x3A, 0x43], [0x2C, 0x53, 0x64]),
        ([0x8E, 0x2D, 0xE2], [0x4A, 0x00, 0xE0], [0x00, 0xD2, 0xFF]),
        ([0xFF, 0x51, 0x2F], [0xDD, 0x24, 0x76], [0xFF, 0xB3, 0x47]),
        ([0x11, 0x99, 0x8E], [0x38, 0xEF, 0x7D], [0xFF, 0xF7, 0xAD]),
        ([0x3A, 0x1C, 0x71], [0xD7, 0x6D, 0x77], [0xFF, 0xAF, 0x7B]),
    ];
    let (c0, c1, c2) = palettes[(seed as usize) % palettes.len()];
    let tau = std::f32::consts::TAU;
    let angle = phase * tau;

    let orb_x = 0.5 + 0.28 * (angle + (seed as f32) * 0.7).cos();
    let orb_y = 0.5 + 0.24 * (angle * 2.0 + (seed as f32) * 0.4).sin();

    for y in 0..h {
        let ny = y as f32 / h.max(1) as f32;
        for x in 0..w {
            let nx = x as f32 / w.max(1) as f32;
            let wave = 0.5 + 0.5 * ((nx * 4.5 + ny * 2.5 + angle).sin());
            let r = lerp_u8(c0[0], c1[0], wave);
            let g = lerp_u8(c0[1], c1[1], wave);
            let b = lerp_u8(c0[2], c1[2], wave);

            let dx = nx - orb_x;
            let dy = (ny - orb_y) * (h as f32 / w.max(1) as f32);
            let d = (dx * dx + dy * dy).sqrt();
            let glow = (1.0 - d * 3.8).clamp(0.0, 1.0).powi(2);

            let idx = ((y * w + x) * 4) as usize;
            out[idx] = lerp_u8(r, c2[0], glow);
            out[idx + 1] = lerp_u8(g, c2[1], glow);
            out[idx + 2] = lerp_u8(b, c2[2], glow);
            out[idx + 3] = 255;
        }
    }
    out
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
}
