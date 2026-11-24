//! Cloud themes, per-chat themes, and cloud wallpapers backend trait & domain records.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_cloud_themes.{h,cpp}`
//! - `Telegram/SourceFiles/data/data_wall_paper.{h,cpp}`
//! - `Telegram/SourceFiles/window/themes/window_theme_editor_box.cpp`
//! - `Telegram/SourceFiles/boxes/background_box.cpp`
//! - `Telegram/SourceFiles/boxes/background_preview_box.cpp`

use tg_theme::{CloudTheme, Color, WallPaper};

/// Reference to a cloud Telegram `Document` attached to a theme or wallpaper.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudDocumentRef {
    /// Document ID.
    pub id: i64,
    /// Document access hash.
    pub access_hash: i64,
    /// File reference token for `upload.getFile` / `InputDocument`.
    pub file_reference: Vec<u8>,
    /// Data center ID hosting the document.
    pub dc_id: i32,
    /// Document MIME type (e.g. `"application/x-tgtheme-tdesktop"` or `"image/jpeg"`).
    pub mime_type: String,
    /// File size in bytes.
    pub size: i64,
}

/// Telegram cloud base theme identifier (`BaseTheme` TL enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloudBaseTheme {
    /// Classic theme (`baseThemeClassic`).
    Classic,
    /// Day theme (`baseThemeDay`).
    Day,
    /// Night theme (`baseThemeNight`).
    Night,
    /// Tinted dark theme (`baseThemeTinted`).
    Tinted,
    /// Arctic light theme (`baseThemeArctic`).
    Arctic,
}

/// Per-variant theme settings transmitted in `account.createTheme` / `account.updateTheme`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudThemeSettingsInput {
    /// Base theme variant.
    pub base_theme: CloudBaseTheme,
    /// Primary accent color.
    pub accent_color: Color,
    /// Optional outgoing message accent color.
    pub outbox_accent_color: Option<Color>,
    /// Optional outgoing message bubble colors (up to 4).
    pub message_colors: Vec<Color>,
    /// Whether message gradient colors animate.
    pub message_colors_animated: bool,
    /// Optional wallpaper paired with this theme variant.
    pub wallpaper: Option<CloudWallPaperRecord>,
}

/// Cloud theme record returned by `account.getThemes`, `account.getChatThemes`,
/// `account.createTheme`, and `account.updateTheme`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudThemeRecord {
    /// Core `tg-theme` cloud theme metadata (`id`, `access_hash`, `slug`, `title`, `emoticon`, `settings`).
    pub theme: CloudTheme,
    /// True if the current account created this theme (`theme.creator`).
    pub is_creator: bool,
    /// True if this is a built-in server default theme (`theme.default`).
    pub is_default: bool,
    /// True if this theme is a per-chat theme (`theme.for_chat`).
    pub for_chat: bool,
    /// Optional `.tdesktop-theme` document reference attached to this cloud theme.
    pub document: Option<CloudDocumentRef>,
}

/// Response from `account.getThemes` or `account.getChatThemes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudThemesResult {
    /// Server confirmed the caller's hash is already up to date.
    NotModified,
    /// Fresh theme catalog and updated cache hash.
    Themes {
        /// New hash to pass on subsequent requests.
        hash: i64,
        /// Returned cloud themes.
        themes: Vec<CloudThemeRecord>,
    },
}

/// Request payload for creating a cloud theme (`account.uploadTheme` + `account.createTheme`).
#[derive(Debug, Clone)]
pub struct CloudThemeCreateRequest {
    /// Unique share slug (`t.me/addtheme/<slug>`).
    pub slug: String,
    /// Human-readable display title.
    pub title: String,
    /// Optional packed `.tdesktop-theme` bytes to upload via `account.uploadTheme`.
    pub theme_bytes: Option<Vec<u8>>,
    /// Optional preview thumbnail bytes (JPEG/PNG) uploaded alongside the theme file.
    pub thumb_bytes: Option<Vec<u8>>,
    /// File name passed to `account.uploadTheme` (e.g. `"my_theme.tdesktop-theme"`).
    pub file_name: String,
    /// Optional structured `InputThemeSettings` variants.
    pub settings: Vec<CloudThemeSettingsInput>,
}

/// Request payload for updating an existing cloud theme (`account.uploadTheme` + `account.updateTheme`).
#[derive(Debug, Clone)]
pub struct CloudThemeUpdateRequest {
    /// Existing server theme ID.
    pub theme_id: u64,
    /// Existing server theme access hash.
    pub access_hash: u64,
    /// Optional updated slug.
    pub slug: Option<String>,
    /// Optional updated title.
    pub title: Option<String>,
    /// Optional new `.tdesktop-theme` bytes to upload via `account.uploadTheme`.
    pub theme_bytes: Option<Vec<u8>>,
    /// Optional updated preview thumbnail bytes.
    pub thumb_bytes: Option<Vec<u8>>,
    /// File name passed to `account.uploadTheme`.
    pub file_name: String,
    /// Optional structured `InputThemeSettings` variants.
    pub settings: Vec<CloudThemeSettingsInput>,
}

/// Request payload for installing/syncing the active theme (`account.installTheme`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudThemeInstallRequest {
    /// Whether the installed theme is a dark/night theme.
    pub dark: bool,
    /// Explicit `(id, access_hash)` for a cloud theme.
    pub theme_id: Option<(u64, u64)>,
    /// Alternative cloud theme slug when ID/access_hash are not known.
    pub slug: Option<String>,
    /// Optional base theme when installing an embedded/base theme.
    pub base_theme: Option<CloudBaseTheme>,
}

/// Chat theme choice passed to `messages.setChatTheme`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatThemeChoice {
    /// Clear per-chat theme (`inputChatThemeEmpty`).
    Empty,
    /// Apply an emoji chat theme (`inputChatTheme`).
    Emoticon(String),
    /// Apply a unique collectible gift chat theme (`inputChatThemeUniqueGift`).
    UniqueGift(String),
}

/// Cloud wallpaper record carrying `tg_theme::WallPaper` plus TL-only fields
/// (`motion`, `is_no_file`, and optional `CloudDocumentRef`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudWallPaperRecord {
    /// Core `tg-theme` wallpaper model (ID, access hash, flags, slug, colors, rotation, intensity, blur).
    pub wallpaper: WallPaper,
    /// Motion animation flag from `wallPaperSettings.motion`.
    pub motion: bool,
    /// Whether the server represented this entry as `wallPaperNoFile`.
    pub is_no_file: bool,
    /// Optional attached image/pattern document reference.
    pub document: Option<CloudDocumentRef>,
}

impl Default for CloudWallPaperRecord {
    fn default() -> Self {
        Self {
            wallpaper: WallPaper::construct_default(),
            motion: false,
            is_no_file: false,
            document: None,
        }
    }
}

/// Response from `account.getWallPapers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudWallPapersResult {
    /// Server confirmed the caller's hash is already up to date.
    NotModified,
    /// Fresh wallpaper catalog and updated cache hash.
    WallPapers {
        /// New hash to pass on subsequent requests.
        hash: i64,
        /// Returned cloud wallpapers.
        wallpapers: Vec<CloudWallPaperRecord>,
    },
}

/// Lookup key for `account.getMultiWallPapers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallPaperLookup {
    /// Lookup by `(id, access_hash)`.
    Id {
        /// Server wallpaper ID.
        id: u64,
        /// Server wallpaper access hash.
        access_hash: u64,
    },
    /// Lookup by share slug (`t.me/bg/<slug>`).
    Slug(String),
    /// Lookup for a color/gradient-only `wallPaperNoFile` ID.
    NoFile(u64),
}

/// Accent peer color option returned by `help.getPeerColors` (`api_peer_colors.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerColorOptionRecord {
    /// Palette index (`0..64`, matching `Ui::kColorIndexCount`).
    pub color_id: u8,
    /// When `true`, excluded from the suggested picker list.
    pub hidden: bool,
    /// Minimum boost level required to use this color in broadcast channels.
    pub channel_min_level: Option<i32>,
    /// Minimum boost level required to use this color in megagroups.
    pub group_min_level: Option<i32>,
    /// 1..=3 RGB outline/accent colors for light mode (`help.peerColorSet`).
    pub light_colors: Vec<Color>,
    /// 1..=3 RGB outline/accent colors for dark mode (falls back to `light_colors` if omitted).
    pub dark_colors: Vec<Color>,
}

/// Response from `help.getPeerColors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerColorsResult {
    /// Server confirmed the caller's `i32` hash is up to date (`help.peerColorsNotModified`).
    NotModified,
    /// Fresh peer accent color catalog and updated cache hash.
    Colors {
        /// New `i32` hash to pass on subsequent requests.
        hash: i32,
        /// Parsed color options (`color_id < 64`).
        options: Vec<PeerColorOptionRecord>,
        /// Non-hidden color IDs in server-suggested picker order.
        suggested: Vec<u8>,
    },
}

/// Profile color triplet set (`Data::ColorProfileSet` / `help.peerColorProfileSet`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerProfileColorSet {
    /// Accent palette colors for profile buttons/controls.
    pub palette: Vec<Color>,
    /// Profile header background gradient colors.
    pub bg: Vec<Color>,
    /// Story ring colors.
    pub story: Vec<Color>,
}

/// Profile peer color option returned by `help.getPeerProfileColors` (`api_peer_colors.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerProfileColorOptionRecord {
    /// Profile color index (`0..64`).
    pub color_id: u8,
    /// When `true`, hidden from suggested pickers.
    pub hidden: bool,
    /// Minimum boost level required in broadcast channels.
    pub channel_min_level: Option<i32>,
    /// Minimum boost level required in megagroups.
    pub group_min_level: Option<i32>,
    /// Light-mode profile color set.
    pub light: PeerProfileColorSet,
    /// Dark-mode profile color set.
    pub dark: PeerProfileColorSet,
}

/// Response from `help.getPeerProfileColors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerProfileColorsResult {
    /// Server confirmed the caller's `i32` hash is up to date (`help.peerColorsNotModified`).
    NotModified,
    /// Fresh profile color catalog and updated cache hash.
    Colors {
        /// New `i32` hash to pass on subsequent requests.
        hash: i32,
        /// Parsed profile color options.
        options: Vec<PeerProfileColorOptionRecord>,
    },
}

/// Collectible unique Star Gift chat theme returned by `account.getUniqueGiftChatThemes`
/// (`data_cloud_themes.cpp`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UniqueGiftChatThemeRecord {
    /// Unique Star Gift ID (`StarGiftUnique.id`).
    pub gift_id: u64,
    /// Unique gift share slug (used with `inputChatThemeUniqueGift`).
    pub slug: String,
    /// Display title of the unique gift.
    pub title: String,
    /// Collectible serial number (`StarGiftUnique.num`).
    pub num: i32,
    /// Parsed `CloudTheme` light/dark settings (`CloudTheme::Parse(MTPDchatThemeUniqueGift)`).
    pub theme: CloudTheme,
}

/// Response from `account.getUniqueGiftChatThemes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UniqueGiftChatThemesResult {
    /// Server confirmed the caller's `i64` hash is up to date (`account.chatThemesNotModified`).
    NotModified,
    /// Page of unique gift chat themes with optional pagination offset.
    Themes {
        /// New `i64` hash for subsequent refresh calls.
        hash: i64,
        /// Parsed unique gift themes in this page.
        themes: Vec<UniqueGiftChatThemeRecord>,
        /// Next pagination cursor (`None` when all pages have been loaded).
        next_offset: Option<String>,
    },
}

/// Backdrop option returned by `messages.getShareThemeBackdrops` (`api_share_themes.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareThemeBackdropOption {
    /// Server-assigned backdrop identifier (`ShareThemeBackdropOption.backdrop_id`).
    pub backdrop_id: i32,
    /// Display name of the backdrop.
    pub name: String,
    /// Center radial gradient color.
    pub center_color: Color,
    /// Outer edge gradient color.
    pub edge_color: Color,
    /// Pattern overlay tint color.
    pub pattern_color: Color,
    /// Foreground text color on this backdrop.
    pub text_color: Color,
}

/// Response from `messages.getShareThemeBackdrops`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareThemeBackdropsResult {
    /// Server confirmed the caller's `i32` hash is up to date (`shareThemeBackdropsNotModified`).
    NotModified,
    /// Fresh share-theme backdrop options and updated hash.
    Backdrops {
        /// New `i32` hash to pass on subsequent requests.
        hash: i32,
        /// Available backdrop presets.
        backdrops: Vec<ShareThemeBackdropOption>,
    },
}

/// Reaction/emoji item attached to a [`ShareThemeChoice::EmojiBackdrop`] (`api_share_themes.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareThemeReaction {
    /// Standard Unicode emoji reaction (`reactionEmoji`).
    Emoji(String),
    /// Custom emoji document ID (`reactionCustomEmoji`).
    CustomEmoji(i64),
}

/// Shareable profile/channel card theme choice passed to `messages.setShareTheme`
/// (`Data::ShareTheme` in `data_peer_share_theme.h`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ShareThemeChoice {
    /// Reset to default share theme (`shareThemeDefault`).
    #[default]
    Default,
    /// Custom backdrop and/or floating emojis (`shareThemeEmojiBackdrop`).
    EmojiBackdrop {
        /// Optional backdrop ID from `messages.getShareThemeBackdrops`.
        backdrop_id: Option<i32>,
        /// Optional floating emoji/custom-emoji reactions.
        emojis: Vec<ShareThemeReaction>,
    },
    /// Collectible Star Gift share theme (`inputShareThemeGift`).
    Gift {
        /// Message ID for a user-saved gift (`inputSavedStarGiftUser`, when non-zero).
        msg_id: i32,
        /// Owning channel/peer ID for a chat-saved gift (`inputSavedStarGiftChat`).
        peer_id: Option<i64>,
        /// Saved gift ID for a chat-saved gift (`inputSavedStarGiftChat`).
        saved_id: i64,
        /// Optional collectible slug (`inputSavedStarGiftSlug`, fallback when no `msg_id`/`saved_id`).
        slug: Option<String>,
    },
}

/// Backend trait connecting [`crate::themes::ThemesStudio`] to Telegram's cloud theme,
/// chat theme, and wallpaper RPCs.
pub trait ThemesBackend: Send + Sync {
    /// Lists cloud themes (`account.getThemes` with `format = "tdesktop"`).
    fn get_themes(&self, hash: i64) -> Result<CloudThemesResult, String>;

    /// Lists available per-chat emoji themes (`account.getChatThemes`).
    fn get_chat_themes(&self, hash: i64) -> Result<CloudThemesResult, String>;

    /// Uploads a `.tdesktop-theme` file (`upload.saveFilePart` + `account.uploadTheme`).
    fn upload_theme(
        &self,
        file_name: &str,
        bytes: &[u8],
        thumb_bytes: Option<&[u8]>,
    ) -> Result<CloudDocumentRef, String>;

    /// Creates a new cloud theme (`account.uploadTheme` if bytes given + `account.createTheme`).
    fn create_theme(&self, req: &CloudThemeCreateRequest) -> Result<CloudThemeRecord, String>;

    /// Updates an existing cloud theme (`account.uploadTheme` if bytes given + `account.updateTheme`).
    fn update_theme(&self, req: &CloudThemeUpdateRequest) -> Result<CloudThemeRecord, String>;

    /// Marks a cloud or base theme as installed on this account (`account.installTheme`).
    fn install_theme(&self, req: &CloudThemeInstallRequest) -> Result<(), String>;

    /// Downloads the raw `.tdesktop-theme` bytes for a cloud theme document (`upload.getFile`).
    fn download_theme_bytes(&self, doc: &CloudDocumentRef) -> Result<Vec<u8>, String>;

    /// Applies or clears a per-chat theme (`messages.setChatTheme`).
    fn set_chat_theme(&self, peer_id: i64, choice: &ChatThemeChoice) -> Result<(), String>;

    /// Lists cloud wallpapers (`account.getWallPapers`).
    fn get_wallpapers(&self, hash: i64) -> Result<CloudWallPapersResult, String>;

    /// Resolves multiple wallpapers in one batch (`account.getMultiWallPapers`).
    fn get_multi_wallpapers(
        &self,
        lookups: &[WallPaperLookup],
    ) -> Result<Vec<CloudWallPaperRecord>, String>;

    /// Saves (`unsave = false`) or removes (`unsave = true`) a cloud wallpaper (`account.saveWallPaper`).
    fn save_wallpaper(&self, record: &CloudWallPaperRecord, unsave: bool) -> Result<(), String>;

    /// Installs a wallpaper as the account's global background (`account.installWallPaper`).
    fn install_wallpaper(&self, record: &CloudWallPaperRecord) -> Result<(), String>;

    /// Uploads a custom wallpaper image (`upload.saveFilePart` + `account.uploadWallPaper`).
    fn upload_wallpaper(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        for_chat: bool,
        record: &CloudWallPaperRecord,
    ) -> Result<CloudWallPaperRecord, String>;

    /// Sets or reverts a per-chat wallpaper (`messages.setChatWallPaper`).
    fn set_chat_wallpaper(
        &self,
        peer_id: i64,
        record: Option<&CloudWallPaperRecord>,
        for_both: bool,
        revert: bool,
        from_message_id: Option<i32>,
    ) -> Result<(), String>;

    /// Downloads the raw image/pattern bytes for a wallpaper document (`upload.getFile`).
    fn download_wallpaper_bytes(&self, doc: &CloudDocumentRef) -> Result<Vec<u8>, String>;

    /// Saves (`unsave = false`) or removes (`unsave = true`) a cloud theme (`account.saveTheme`).
    fn save_theme(&self, _theme_id: u64, _access_hash: u64, _unsave: bool) -> Result<(), String> {
        Err("Saving or removing cloud themes is not supported by this backend.".to_owned())
    }

    /// Lists collectible unique Star Gift chat themes (`account.getUniqueGiftChatThemes`).
    fn get_unique_gift_chat_themes(
        &self,
        _offset: &str,
        _limit: i32,
        _hash: i64,
    ) -> Result<UniqueGiftChatThemesResult, String> {
        Ok(UniqueGiftChatThemesResult::Themes {
            hash: 0,
            themes: Vec::new(),
            next_offset: None,
        })
    }

    /// Lists share-card theme backdrop presets (`messages.getShareThemeBackdrops`).
    fn get_share_theme_backdrops(&self, _hash: i32) -> Result<ShareThemeBackdropsResult, String> {
        Ok(ShareThemeBackdropsResult::Backdrops {
            hash: 0,
            backdrops: Vec::new(),
        })
    }

    /// Sets or resets a peer's share-card theme (`messages.setShareTheme`).
    fn set_share_theme(&self, _peer_id: i64, _theme: &ShareThemeChoice) -> Result<(), String> {
        Err("Updating share themes is not supported by this backend.".to_owned())
    }

    /// Lists peer accent/name colors (`help.getPeerColors`).
    fn get_peer_colors(&self, _hash: i32) -> Result<PeerColorsResult, String> {
        Ok(PeerColorsResult::Colors {
            hash: 0,
            options: Vec::new(),
            suggested: Vec::new(),
        })
    }

    /// Lists peer profile header colors (`help.getPeerProfileColors`).
    fn get_peer_profile_colors(&self, _hash: i32) -> Result<PeerProfileColorsResult, String> {
        Ok(PeerProfileColorsResult::Colors {
            hash: 0,
            options: Vec::new(),
        })
    }
}
