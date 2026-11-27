//! Live Telegram cloud themes, per-chat themes, and cloud wallpapers RPC adapter for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_cloud_themes.cpp`
//! - `Telegram/SourceFiles/data/data_wall_paper.cpp`
//! - `Telegram/SourceFiles/window/themes/window_theme.cpp`
//! - `Telegram/SourceFiles/window/themes/window_theme_editor_box.cpp`
//! - `Telegram/SourceFiles/boxes/background_box.cpp`
//! - `Telegram/SourceFiles/boxes/background_preview_box.cpp`
//!
//! Implements [`ThemesBackend`] for [`LiveSession`] covering:
//! - Cloud themes: `account.getThemes`, `account.createTheme`, `account.updateTheme`,
//!   `account.installTheme`, `account.uploadTheme`
//! - Chat themes: `account.getChatThemes`, `messages.setChatTheme`
//! - Wallpapers: `account.getWallPapers`, `account.saveWallPaper`, `account.installWallPaper`,
//!   `account.uploadWallPaper`, `account.getMultiWallPapers`, `messages.setChatWallPaper`

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::cached_lists::{CachedPeerColorOption, PEER_COLORS_REFRESH_MS, PeerColorsSync};
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for};
use tg_data::Session;
use tg_theme::{CloudTheme, CloudThemeSettings, CloudThemeType, Color, WallPaper, WallPaperFlags};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::to_data_peer;
use crate::media::{block_on_runtime, download_file_bytes, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::stand_in::types::PeerId;
use tg_app_core::themes::backend::{
    ChatThemeChoice, CloudBaseTheme, CloudDocumentRef, CloudThemeCreateRequest,
    CloudThemeInstallRequest, CloudThemeRecord, CloudThemeSettingsInput, CloudThemeUpdateRequest,
    CloudThemesResult, CloudWallPaperRecord, CloudWallPapersResult, PeerColorOptionRecord,
    PeerColorsResult, PeerProfileColorOptionRecord, PeerProfileColorSet, PeerProfileColorsResult,
    ShareThemeBackdropOption, ShareThemeBackdropsResult, ShareThemeChoice, ShareThemeReaction,
    ThemesBackend, UniqueGiftChatThemeRecord, UniqueGiftChatThemesResult, WallPaperLookup,
};

/// Platform theme format string (`Data::CloudThemes::Format()` in `data_cloud_themes.cpp:242`).
pub const CLOUD_THEME_FORMAT: &str = "tdesktop";

/// MIME type for uploaded `.tdesktop-theme` files (`window_theme_editor_box.cpp:591`).
pub const CLOUD_THEME_MIME_TYPE: &str = "application/x-tgtheme-tdesktop";

/// Default page limit for `account.getUniqueGiftChatThemes` (`kGiftThemesLimit` in `data_cloud_themes.cpp:32`).
pub const UNIQUE_GIFT_THEMES_DEFAULT_LIMIT: i32 = 24;

/// Maximum peer color index (`Ui::kColorIndexCount = 1 << 6` in `chat_style.h:52`).
pub const PEER_COLOR_INDEX_COUNT: i32 = 64;

/// Maximum outline colors in a `help.peerColorSet` (`Ui::kColorPatternsCount = 3` in `text.h:210`).
pub const PEER_COLOR_PATTERNS_COUNT: usize = 3;

/// Default timeout for cloud theme and wallpaper RPC invocations.
const THEMES_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum size for small-file uploads (`upload.saveFilePart` vs `upload.saveBigFilePart`).
const SMALL_UPLOAD_LIMIT: usize = 10 * 1024 * 1024;

/// Chunk size for theme/wallpaper uploads (512 KiB max allowed by Telegram).
const UPLOAD_PART_SIZE: usize = 512 * 1024;

static NEXT_UPLOAD_FILE_ID: AtomicI64 = AtomicI64::new(0x5448_454D_0000_0001);

// =============================================================================
// Per-session theme/wallpaper cache (keeps LiveSession struct untouched)
// =============================================================================

#[derive(Default)]
struct LiveThemesCache {
    themes_hash: i64,
    themes: Vec<CloudThemeRecord>,
    chat_themes_hash: i64,
    chat_themes: Vec<CloudThemeRecord>,
    wallpapers_hash: i64,
    wallpapers: Vec<CloudWallPaperRecord>,
    unique_gift_themes_hash: i64,
    unique_gift_themes: Vec<UniqueGiftChatThemeRecord>,
    unique_gift_themes_next_offset: Option<String>,
    share_theme_backdrops_hash: i32,
    share_theme_backdrops: Vec<ShareThemeBackdropOption>,
    peer_share_themes: HashMap<i64, ShareThemeChoice>,
    /// Hash & 1-hour cadence sync state shared with `tg_api::cached_lists::PeerColorsSync`.
    peer_colors_sync: PeerColorsSync,
    peer_colors: Vec<PeerColorOptionRecord>,
    peer_colors_suggested: Vec<u8>,
    peer_profile_colors: Vec<PeerProfileColorOptionRecord>,
}

struct ThemesCacheEntry {
    owner: Weak<LiveBridgeState>,
    state: LiveThemesCache,
}

fn themes_cache_registry() -> &'static Mutex<HashMap<usize, ThemesCacheEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, ThemesCacheEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_themes_cache<R>(live: &LiveSession, f: impl FnOnce(&mut LiveThemesCache) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = themes_cache_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| ThemesCacheEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: LiveThemesCache::default(),
    });
    f(&mut entry.state)
}

/// Clears cached cloud theme/wallpaper catalogs for `live`.
pub(crate) fn invalidate_cache(live: &LiveSession) {
    with_themes_cache(live, |c| {
        c.themes_hash = 0;
        c.themes.clear();
        c.chat_themes_hash = 0;
        c.chat_themes.clear();
        c.wallpapers_hash = 0;
        c.wallpapers.clear();
        c.unique_gift_themes_hash = 0;
        c.unique_gift_themes.clear();
        c.unique_gift_themes_next_offset = None;
        c.share_theme_backdrops_hash = 0;
        c.share_theme_backdrops.clear();
        c.peer_share_themes.clear();
        c.peer_colors_sync = PeerColorsSync::default();
        c.peer_colors.clear();
        c.peer_colors_suggested.clear();
        c.peer_profile_colors.clear();
    });
}

// =============================================================================
// Internal RPC, Session & Upload helpers
// =============================================================================

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(THEMES_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn apply_updates_users_chats(live: &LiveSession, updates: &tl_enums::Updates) {
    let _ = live.apply_updates_to_session(updates);
}

fn resolve_input_peer(live: &LiveSession, peer_id: i64) -> Result<tl_enums::InputPeer, String> {
    let data_peer = to_data_peer(PeerId(peer_id));
    let input = read_session(live, |s| input_peer_for(s, data_peer));
    if matches!(input, tl_enums::InputPeer::Empty) {
        Err(format!("Unknown peer {peer_id} for theme/wallpaper update"))
    } else {
        Ok(input)
    }
}

fn upload_raw_file(
    live: &LiveSession,
    bytes: &[u8],
    file_name: &str,
) -> Result<tl_enums::InputFile, String> {
    if bytes.is_empty() {
        return Err("Cannot upload empty theme/wallpaper payload".to_owned());
    }
    let file_id = NEXT_UPLOAD_FILE_ID.fetch_add(1, Ordering::Relaxed);
    let total_parts = bytes.len().div_ceil(UPLOAD_PART_SIZE);
    let is_big = bytes.len() > SMALL_UPLOAD_LIMIT;

    for (part_idx, chunk) in bytes.chunks(UPLOAD_PART_SIZE).enumerate() {
        let ok = if is_big {
            invoke_rpc(
                live,
                tl_fns::upload::SaveBigFilePart {
                    file_id,
                    file_part: part_idx as i32,
                    file_total_parts: total_parts as i32,
                    bytes: chunk.to_vec(),
                },
            )?
        } else {
            invoke_rpc(
                live,
                tl_fns::upload::SaveFilePart {
                    file_id,
                    file_part: part_idx as i32,
                    bytes: chunk.to_vec(),
                },
            )?
        };
        if !ok {
            return Err(format!("Server rejected upload part {part_idx}"));
        }
    }

    if is_big {
        Ok(tl_enums::InputFile::Big(tl_types::InputFileBig {
            id: file_id,
            parts: total_parts as i32,
            name: file_name.to_owned(),
        }))
    } else {
        Ok(tl_enums::InputFile::InputFile(tl_types::InputFile {
            id: file_id,
            parts: total_parts as i32,
            name: file_name.to_owned(),
            md5_checksum: String::new(),
        }))
    }
}

// =============================================================================
// TL <-> Domain conversions (`data_wall_paper.cpp`, `data_cloud_themes.cpp`)
// =============================================================================

/// Serializes an RGB [`Color`] into Telegram's 24-bit integer format (`SerializeColor` in `data_wall_paper.cpp:57`).
#[must_use]
pub const fn serialize_tl_color(color: Color) -> i32 {
    (((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32)) as i32
}

/// Deserializes a Telegram 24-bit RGB integer into an opaque [`Color`] (`ColorFromSerialized`).
#[must_use]
pub const fn deserialize_tl_color(raw: i32) -> Color {
    let bits = raw as u32;
    Color::rgb(
        ((bits >> 16) & 0xFF) as u8,
        ((bits >> 8) & 0xFF) as u8,
        (bits & 0xFF) as u8,
    )
}

/// Converts a [`CloudBaseTheme`] to TL [`tl_enums::BaseTheme`].
#[must_use]
pub fn base_theme_to_tl(base: CloudBaseTheme) -> tl_enums::BaseTheme {
    match base {
        CloudBaseTheme::Classic => tl_enums::BaseTheme::Classic,
        CloudBaseTheme::Day => tl_enums::BaseTheme::Day,
        CloudBaseTheme::Night => tl_enums::BaseTheme::Night,
        CloudBaseTheme::Tinted => tl_enums::BaseTheme::Tinted,
        CloudBaseTheme::Arctic => tl_enums::BaseTheme::Arctic,
    }
}

/// Maps a TL [`tl_enums::BaseTheme`] into [`CloudThemeType`] (`Dark` vs `Light`).
#[must_use]
pub const fn base_theme_to_cloud_type(base: &tl_enums::BaseTheme) -> CloudThemeType {
    match base {
        tl_enums::BaseTheme::Night | tl_enums::BaseTheme::Tinted => CloudThemeType::Dark,
        tl_enums::BaseTheme::Classic | tl_enums::BaseTheme::Day | tl_enums::BaseTheme::Arctic => {
            CloudThemeType::Light
        }
    }
}

/// Converts a TL [`tl_enums::Document`] into [`CloudDocumentRef`].
#[must_use]
pub fn document_from_tl(doc: &tl_enums::Document) -> Option<CloudDocumentRef> {
    match doc {
        tl_enums::Document::Document(d) => Some(CloudDocumentRef {
            id: d.id,
            access_hash: d.access_hash,
            file_reference: d.file_reference.clone(),
            dc_id: d.dc_id,
            mime_type: d.mime_type.clone(),
            size: d.size,
        }),
        tl_enums::Document::Empty(_) => None,
    }
}

/// Converts a [`CloudWallPaperRecord`] into TL [`tl_enums::WallPaperSettings`] preserving
/// blur, motion, pattern intensity, gradient colors (1..=4), rotation, and emoticon.
#[must_use]
pub fn wallpaper_settings_to_tl(record: &CloudWallPaperRecord) -> tl_enums::WallPaperSettings {
    let wp = &record.wallpaper;
    let color_at = |idx: usize| {
        wp.background_colors
            .get(idx)
            .copied()
            .map(serialize_tl_color)
    };
    let rotation = if wp.background_colors.len() >= 2 || wp.rotation != 0 {
        Some((wp.rotation.rem_euclid(360) / 45) * 45)
    } else {
        None
    };

    tl_enums::WallPaperSettings::WallPaperSettings(tl_types::WallPaperSettings {
        blur: wp.blurred,
        motion: record.motion,
        background_color: color_at(0),
        second_background_color: color_at(1),
        third_background_color: color_at(2),
        fourth_background_color: color_at(3),
        intensity: Some(wp.intensity.clamp(-100, 100)),
        rotation,
        emoticon: (!wp.emoji_id.is_empty()).then(|| wp.emoji_id.clone()),
    })
}

/// Converts a [`CloudWallPaperRecord`] into TL [`tl_enums::InputWallPaper`] matching
/// `WallPaper::mtpInput` in `data_wall_paper.cpp:336-340`.
#[must_use]
pub fn wallpaper_to_input_tl(
    record: &CloudWallPaperRecord,
    self_user_id: u64,
) -> tl_enums::InputWallPaper {
    let wp = &record.wallpaper;
    if record.is_no_file {
        return tl_enums::InputWallPaper::NoFile(tl_types::InputWallPaperNoFile {
            id: wp.id as i64,
        });
    }
    if wp.owner_id != 0 && wp.owner_id != self_user_id && !wp.slug.is_empty() {
        return tl_enums::InputWallPaper::Slug(tl_types::InputWallPaperSlug {
            slug: wp.slug.clone(),
        });
    }
    if wp.id != 0 && wp.access_hash != 0 {
        return tl_enums::InputWallPaper::InputWallPaper(tl_types::InputWallPaper {
            id: wp.id as i64,
            access_hash: wp.access_hash as i64,
        });
    }
    if !wp.slug.is_empty() {
        return tl_enums::InputWallPaper::Slug(tl_types::InputWallPaperSlug {
            slug: wp.slug.clone(),
        });
    }
    tl_enums::InputWallPaper::NoFile(tl_types::InputWallPaperNoFile { id: wp.id as i64 })
}

fn parse_wallpaper_colors(settings: &tl_types::WallPaperSettings) -> Vec<Color> {
    let mut colors = Vec::new();
    let Some(c1) = settings.background_color else {
        return colors;
    };
    colors.push(deserialize_tl_color(c1));
    let Some(c2) = settings.second_background_color else {
        return colors;
    };
    colors.push(deserialize_tl_color(c2));
    let Some(c3) = settings.third_background_color else {
        return colors;
    };
    colors.push(deserialize_tl_color(c3));
    if let Some(c4) = settings.fourth_background_color {
        colors.push(deserialize_tl_color(c4));
    }
    colors
}

/// Converts a TL [`tl_enums::WallPaper`] into [`CloudWallPaperRecord`] matching
/// `WallPaper::Create(session, MTPWallPaper)` in `data_wall_paper.cpp:475-520`.
#[must_use]
pub fn wallpaper_from_tl(wp: &tl_enums::WallPaper) -> CloudWallPaperRecord {
    match wp {
        tl_enums::WallPaper::WallPaper(data) => {
            let mut flags = WallPaperFlags::EMPTY;
            if data.creator {
                flags = flags.union(WallPaperFlags::CREATOR);
            }
            if data.default {
                flags = flags.union(WallPaperFlags::DEFAULT);
            }
            if data.pattern {
                flags = flags.union(WallPaperFlags::PATTERN);
            }
            if data.dark {
                flags = flags.union(WallPaperFlags::DARK);
            }
            let doc = document_from_tl(&data.document);
            let mut paper = WallPaper::new(data.id as u64);
            paper.access_hash = data.access_hash as u64;
            paper.flags = flags;
            paper.slug = data.slug.clone();
            paper.document_id = doc.as_ref().map(|d| d.id as u64);

            let mut motion = false;
            if let Some(tl_enums::WallPaperSettings::WallPaperSettings(s)) = &data.settings {
                paper.blurred = s.blur;
                motion = s.motion;
                if let Some(intensity) = s.intensity {
                    paper.intensity = intensity;
                }
                if paper.is_pattern() {
                    paper.background_colors = parse_wallpaper_colors(s);
                } else {
                    let parsed_colors = parse_wallpaper_colors(s);
                    if !parsed_colors.is_empty() {
                        paper.background_colors = parsed_colors;
                    }
                }
                if let Some(rot) = s.rotation {
                    paper.rotation = (rot.rem_euclid(360) / 45) * 45;
                }
                if let Some(ref em) = s.emoticon {
                    paper.emoji_id = em.clone();
                }
            }

            CloudWallPaperRecord {
                wallpaper: paper,
                motion,
                is_no_file: false,
                document: doc,
            }
        }
        tl_enums::WallPaper::NoFile(data) => {
            let mut flags = WallPaperFlags::EMPTY;
            if data.default {
                flags = flags.union(WallPaperFlags::DEFAULT);
            }
            if data.dark {
                flags = flags.union(WallPaperFlags::DARK);
            }
            let mut paper = WallPaper::new(data.id as u64);
            paper.flags = flags;
            let mut motion = false;
            if let Some(tl_enums::WallPaperSettings::WallPaperSettings(s)) = &data.settings {
                paper.blurred = s.blur;
                motion = s.motion;
                paper.background_colors = parse_wallpaper_colors(s);
                if let Some(intensity) = s.intensity {
                    paper.intensity = intensity;
                }
                if let Some(rot) = s.rotation {
                    paper.rotation = (rot.rem_euclid(360) / 45) * 45;
                }
                if let Some(ref em) = s.emoticon {
                    paper.emoji_id = em.clone();
                }
            }
            CloudWallPaperRecord {
                wallpaper: paper,
                motion,
                is_no_file: true,
                document: None,
            }
        }
    }
}

/// Converts a TL [`tl_enums::Theme`] into [`CloudThemeRecord`] matching
/// `CloudTheme::Parse` in `data_cloud_themes.cpp`.
#[must_use]
pub fn cloud_theme_from_tl(theme: &tl_enums::Theme, self_user_id: u64) -> CloudThemeRecord {
    let tl_enums::Theme::Theme(t) = theme;
    let doc = t.document.as_ref().and_then(document_from_tl);
    let mut settings_map: BTreeMap<CloudThemeType, CloudThemeSettings> = BTreeMap::new();
    if let Some(ref list) = t.settings {
        for item in list {
            let tl_enums::ThemeSettings::ThemeSettings(ts) = item;
            let variant = base_theme_to_cloud_type(&ts.base_theme);
            let paper = ts
                .wallpaper
                .as_ref()
                .map(|w| wallpaper_from_tl(w).wallpaper);
            let outgoing_messages_colors = ts
                .message_colors
                .as_ref()
                .map(|cols| cols.iter().copied().map(deserialize_tl_color).collect())
                .unwrap_or_default();
            settings_map.insert(
                variant,
                CloudThemeSettings {
                    paper,
                    accent_color: deserialize_tl_color(ts.accent_color),
                    outgoing_accent_color: ts.outbox_accent_color.map(deserialize_tl_color),
                    outgoing_messages_colors,
                },
            );
        }
    }

    CloudThemeRecord {
        theme: CloudTheme {
            id: t.id as u64,
            access_hash: t.access_hash as u64,
            slug: t.slug.clone(),
            title: t.title.clone(),
            document_id: doc.as_ref().map_or(0, |d| d.id as u64),
            created_by: if t.creator { self_user_id } else { 0 },
            users_count: t.installs_count.unwrap_or(0),
            emoticon: t.emoticon.clone().unwrap_or_default(),
            settings: settings_map,
        },
        is_creator: t.creator,
        is_default: t.default,
        for_chat: t.for_chat,
        document: doc,
    }
}

fn theme_settings_input_to_tl(
    input: &CloudThemeSettingsInput,
    self_user_id: u64,
) -> tl_enums::InputThemeSettings {
    let (wallpaper, wallpaper_settings) = match &input.wallpaper {
        Some(wp) => (
            Some(wallpaper_to_input_tl(wp, self_user_id)),
            Some(wallpaper_settings_to_tl(wp)),
        ),
        None => (None, None),
    };
    let message_colors = if input.message_colors.is_empty() {
        None
    } else {
        Some(
            input
                .message_colors
                .iter()
                .copied()
                .map(serialize_tl_color)
                .collect(),
        )
    };
    tl_enums::InputThemeSettings::InputThemeSettings(tl_types::InputThemeSettings {
        message_colors_animated: input.message_colors_animated,
        base_theme: base_theme_to_tl(input.base_theme),
        accent_color: serialize_tl_color(input.accent_color),
        outbox_accent_color: input.outbox_accent_color.map(serialize_tl_color),
        message_colors,
        wallpaper,
        wallpaper_settings,
    })
}

fn wallpaper_lookup_to_tl(lookup: &WallPaperLookup) -> tl_enums::InputWallPaper {
    match lookup {
        WallPaperLookup::Id { id, access_hash } => {
            tl_enums::InputWallPaper::InputWallPaper(tl_types::InputWallPaper {
                id: *id as i64,
                access_hash: *access_hash as i64,
            })
        }
        WallPaperLookup::Slug(slug) => {
            tl_enums::InputWallPaper::Slug(tl_types::InputWallPaperSlug { slug: slug.clone() })
        }
        WallPaperLookup::NoFile(id) => {
            tl_enums::InputWallPaper::NoFile(tl_types::InputWallPaperNoFile { id: *id as i64 })
        }
    }
}

// =============================================================================
// ThemesBackend implementation for LiveSession
// =============================================================================

impl ThemesBackend for LiveSession {
    fn get_themes(&self, hash: i64) -> Result<CloudThemesResult, String> {
        let self_uid = read_session(self, |s| s.self_user_id().0);
        let resp = invoke_rpc(
            self,
            tl_fns::account::GetThemes {
                format: CLOUD_THEME_FORMAT.to_owned(),
                hash,
            },
        )?;
        match resp {
            tl_enums::account::Themes::NotModified => Ok(CloudThemesResult::NotModified),
            tl_enums::account::Themes::Themes(data) => {
                let themes: Vec<CloudThemeRecord> = data
                    .themes
                    .iter()
                    .map(|t| cloud_theme_from_tl(t, self_uid))
                    .collect();
                with_themes_cache(self, |c| {
                    c.themes_hash = data.hash;
                    c.themes = themes.clone();
                });
                self.bridge_state().notify_ui();
                Ok(CloudThemesResult::Themes {
                    hash: data.hash,
                    themes,
                })
            }
        }
    }

    fn get_chat_themes(&self, hash: i64) -> Result<CloudThemesResult, String> {
        let self_uid = read_session(self, |s| s.self_user_id().0);
        let resp = invoke_rpc(self, tl_fns::account::GetChatThemes { hash })?;
        match resp {
            tl_enums::account::Themes::NotModified => Ok(CloudThemesResult::NotModified),
            tl_enums::account::Themes::Themes(data) => {
                let themes: Vec<CloudThemeRecord> = data
                    .themes
                    .iter()
                    .map(|t| cloud_theme_from_tl(t, self_uid))
                    .collect();
                with_themes_cache(self, |c| {
                    c.chat_themes_hash = data.hash;
                    c.chat_themes = themes.clone();
                });
                self.bridge_state().notify_ui();
                Ok(CloudThemesResult::Themes {
                    hash: data.hash,
                    themes,
                })
            }
        }
    }

    fn upload_theme(
        &self,
        file_name: &str,
        bytes: &[u8],
        thumb_bytes: Option<&[u8]>,
    ) -> Result<CloudDocumentRef, String> {
        let file = upload_raw_file(self, bytes, file_name)?;
        let thumb = match thumb_bytes {
            Some(tb) if !tb.is_empty() => Some(upload_raw_file(self, tb, "theme_preview.png")?),
            _ => None,
        };
        let doc = invoke_rpc(
            self,
            tl_fns::account::UploadTheme {
                file,
                thumb,
                file_name: file_name.to_owned(),
                mime_type: CLOUD_THEME_MIME_TYPE.to_owned(),
            },
        )?;
        document_from_tl(&doc)
            .ok_or_else(|| "account.uploadTheme returned DocumentEmpty".to_owned())
    }

    fn create_theme(&self, req: &CloudThemeCreateRequest) -> Result<CloudThemeRecord, String> {
        let self_uid = read_session(self, |s| s.self_user_id().0);
        let document = if let Some(ref bytes) = req.theme_bytes {
            let uploaded = self.upload_theme(&req.file_name, bytes, req.thumb_bytes.as_deref())?;
            Some(tl_enums::InputDocument::InputDocument(
                tl_types::InputDocument {
                    id: uploaded.id,
                    access_hash: uploaded.access_hash,
                    file_reference: uploaded.file_reference,
                },
            ))
        } else {
            None
        };
        let settings = if req.settings.is_empty() {
            None
        } else {
            Some(
                req.settings
                    .iter()
                    .map(|s| theme_settings_input_to_tl(s, self_uid))
                    .collect(),
            )
        };
        let resp = invoke_rpc(
            self,
            tl_fns::account::CreateTheme {
                slug: req.slug.clone(),
                title: req.title.clone(),
                document,
                settings,
            },
        )?;
        let record = cloud_theme_from_tl(&resp, self_uid);
        with_themes_cache(self, |c| {
            c.themes.retain(|t| t.theme.id != record.theme.id);
            c.themes.insert(0, record.clone());
        });
        self.bridge_state().notify_ui();
        Ok(record)
    }

    fn update_theme(&self, req: &CloudThemeUpdateRequest) -> Result<CloudThemeRecord, String> {
        let self_uid = read_session(self, |s| s.self_user_id().0);
        let document = if let Some(ref bytes) = req.theme_bytes {
            let uploaded = self.upload_theme(&req.file_name, bytes, req.thumb_bytes.as_deref())?;
            Some(tl_enums::InputDocument::InputDocument(
                tl_types::InputDocument {
                    id: uploaded.id,
                    access_hash: uploaded.access_hash,
                    file_reference: uploaded.file_reference,
                },
            ))
        } else {
            None
        };
        let settings = if req.settings.is_empty() {
            None
        } else {
            Some(
                req.settings
                    .iter()
                    .map(|s| theme_settings_input_to_tl(s, self_uid))
                    .collect(),
            )
        };
        let resp = invoke_rpc(
            self,
            tl_fns::account::UpdateTheme {
                format: CLOUD_THEME_FORMAT.to_owned(),
                theme: tl_enums::InputTheme::InputTheme(tl_types::InputTheme {
                    id: req.theme_id as i64,
                    access_hash: req.access_hash as i64,
                }),
                slug: req.slug.clone(),
                title: req.title.clone(),
                document,
                settings,
            },
        )?;
        let record = cloud_theme_from_tl(&resp, self_uid);
        with_themes_cache(self, |c| {
            if let Some(existing) = c.themes.iter_mut().find(|t| t.theme.id == record.theme.id) {
                *existing = record.clone();
            } else {
                c.themes.insert(0, record.clone());
            }
        });
        self.bridge_state().notify_ui();
        Ok(record)
    }

    fn install_theme(&self, req: &CloudThemeInstallRequest) -> Result<(), String> {
        let theme = if let Some((id, access_hash)) = req.theme_id {
            Some(tl_enums::InputTheme::InputTheme(tl_types::InputTheme {
                id: id as i64,
                access_hash: access_hash as i64,
            }))
        } else {
            req.slug.as_ref().map(|slug| {
                tl_enums::InputTheme::Slug(tl_types::InputThemeSlug { slug: slug.clone() })
            })
        };
        // Match `CloudThemes::install` (`data_cloud_themes.cpp:294-303`):
        // `Flag::f_format` is always set to `"tdesktop"` even when clearing the cloud theme
        // or selecting a base theme, so the server associates the choice with Desktop.
        let format = Some(CLOUD_THEME_FORMAT.to_owned());
        let base_theme = req.base_theme.map(base_theme_to_tl);
        let _ = invoke_rpc(
            self,
            tl_fns::account::InstallTheme {
                dark: req.dark,
                theme,
                format,
                base_theme,
            },
        )?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn download_theme_bytes(&self, doc: &CloudDocumentRef) -> Result<Vec<u8>, String> {
        let location = tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: doc.id,
                access_hash: doc.access_hash,
                file_reference: doc.file_reference.clone(),
                thumb_size: String::new(),
            },
        );
        download_file_bytes(
            self.runtime().rpc(),
            self.runtime().tokio_handle(),
            doc.dc_id,
            location,
            (doc.size > 0).then_some(doc.size as u64),
        )
        .map_err(|e| e.to_string())
    }

    fn set_chat_theme(&self, peer_id: i64, choice: &ChatThemeChoice) -> Result<(), String> {
        let peer = resolve_input_peer(self, peer_id)?;
        let theme = match choice {
            ChatThemeChoice::Empty => tl_enums::InputChatTheme::Empty,
            ChatThemeChoice::Emoticon(emoticon) => {
                if emoticon.is_empty() {
                    tl_enums::InputChatTheme::Empty
                } else {
                    tl_enums::InputChatTheme::InputChatTheme(tl_types::InputChatTheme {
                        emoticon: emoticon.clone(),
                    })
                }
            }
            ChatThemeChoice::UniqueGift(slug) => {
                tl_enums::InputChatTheme::UniqueGift(tl_types::InputChatThemeUniqueGift {
                    slug: slug.clone(),
                })
            }
        };
        let updates = invoke_rpc(self, tl_fns::messages::SetChatTheme { peer, theme })?;
        apply_updates_users_chats(self, &updates);
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn get_wallpapers(&self, hash: i64) -> Result<CloudWallPapersResult, String> {
        let resp = invoke_rpc(self, tl_fns::account::GetWallPapers { hash })?;
        match resp {
            tl_enums::account::WallPapers::NotModified => Ok(CloudWallPapersResult::NotModified),
            tl_enums::account::WallPapers::WallPapers(data) => {
                let wallpapers: Vec<CloudWallPaperRecord> =
                    data.wallpapers.iter().map(wallpaper_from_tl).collect();
                with_themes_cache(self, |c| {
                    c.wallpapers_hash = data.hash;
                    c.wallpapers = wallpapers.clone();
                });
                self.bridge_state().notify_ui();
                Ok(CloudWallPapersResult::WallPapers {
                    hash: data.hash,
                    wallpapers,
                })
            }
        }
    }

    fn get_multi_wallpapers(
        &self,
        lookups: &[WallPaperLookup],
    ) -> Result<Vec<CloudWallPaperRecord>, String> {
        if lookups.is_empty() {
            return Ok(Vec::new());
        }
        let wallpapers: Vec<tl_enums::InputWallPaper> =
            lookups.iter().map(wallpaper_lookup_to_tl).collect();
        let resp = invoke_rpc(self, tl_fns::account::GetMultiWallPapers { wallpapers })?;
        let parsed: Vec<CloudWallPaperRecord> = resp.iter().map(wallpaper_from_tl).collect();
        self.bridge_state().notify_ui();
        Ok(parsed)
    }

    fn save_wallpaper(&self, record: &CloudWallPaperRecord, unsave: bool) -> Result<(), String> {
        let self_uid = read_session(self, |s| s.self_user_id().0);
        let _ = invoke_rpc(
            self,
            tl_fns::account::SaveWallPaper {
                wallpaper: wallpaper_to_input_tl(record, self_uid),
                unsave,
                settings: wallpaper_settings_to_tl(record),
            },
        )?;
        with_themes_cache(self, |c| {
            if unsave {
                c.wallpapers
                    .retain(|w| w.wallpaper.id != record.wallpaper.id);
            } else if !c
                .wallpapers
                .iter()
                .any(|w| w.wallpaper.id == record.wallpaper.id)
            {
                c.wallpapers.insert(0, record.clone());
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn install_wallpaper(&self, record: &CloudWallPaperRecord) -> Result<(), String> {
        let self_uid = read_session(self, |s| s.self_user_id().0);
        let _ = invoke_rpc(
            self,
            tl_fns::account::InstallWallPaper {
                wallpaper: wallpaper_to_input_tl(record, self_uid),
                settings: wallpaper_settings_to_tl(record),
            },
        )?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn upload_wallpaper(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        for_chat: bool,
        record: &CloudWallPaperRecord,
    ) -> Result<CloudWallPaperRecord, String> {
        let file_name = if mime_type.contains("png") {
            "wallpaper.png"
        } else {
            "wallpaper.jpg"
        };
        let file = upload_raw_file(self, image_bytes, file_name)?;
        let resp = invoke_rpc(
            self,
            tl_fns::account::UploadWallPaper {
                for_chat,
                file,
                mime_type: mime_type.to_owned(),
                settings: wallpaper_settings_to_tl(record),
            },
        )?;
        let parsed = wallpaper_from_tl(&resp);
        if !for_chat {
            with_themes_cache(self, |c| {
                c.wallpapers
                    .retain(|w| w.wallpaper.id != parsed.wallpaper.id);
                c.wallpapers.insert(0, parsed.clone());
            });
        }
        self.bridge_state().notify_ui();
        Ok(parsed)
    }

    fn set_chat_wallpaper(
        &self,
        peer_id: i64,
        record: Option<&CloudWallPaperRecord>,
        for_both: bool,
        revert: bool,
        from_message_id: Option<i32>,
    ) -> Result<(), String> {
        let peer = resolve_input_peer(self, peer_id)?;
        let self_uid = read_session(self, |s| s.self_user_id().0);
        let (wallpaper, settings) = match (revert, from_message_id, record) {
            (true, _, _) => (None, None),
            (false, Some(_), Some(rec)) => (None, Some(wallpaper_settings_to_tl(rec))),
            (false, None, Some(rec)) => (
                Some(wallpaper_to_input_tl(rec, self_uid)),
                Some(wallpaper_settings_to_tl(rec)),
            ),
            (false, _, None) => (None, None),
        };
        let updates = invoke_rpc(
            self,
            tl_fns::messages::SetChatWallPaper {
                for_both,
                revert,
                peer,
                wallpaper,
                settings,
                id: from_message_id,
            },
        )?;
        apply_updates_users_chats(self, &updates);
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn download_wallpaper_bytes(&self, doc: &CloudDocumentRef) -> Result<Vec<u8>, String> {
        let location = tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: doc.id,
                access_hash: doc.access_hash,
                file_reference: doc.file_reference.clone(),
                thumb_size: String::new(),
            },
        );
        download_file_bytes(
            self.runtime().rpc(),
            self.runtime().tokio_handle(),
            doc.dc_id,
            location,
            (doc.size > 0).then_some(doc.size as u64),
        )
        .map_err(|e| e.to_string())
    }

    fn save_theme(&self, theme_id: u64, access_hash: u64, unsave: bool) -> Result<(), String> {
        // Port of `CloudThemes::remove` / `account.saveTheme` (`data_cloud_themes.cpp:888-901`).
        let _ = invoke_rpc(
            self,
            tl_fns::account::SaveTheme {
                theme: tl_enums::InputTheme::InputTheme(tl_types::InputTheme {
                    id: theme_id as i64,
                    access_hash: access_hash as i64,
                }),
                unsave,
            },
        )?;
        if unsave {
            with_themes_cache(self, |c| {
                c.themes.retain(|t| t.theme.id != theme_id);
            });
        }
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn get_unique_gift_chat_themes(
        &self,
        offset: &str,
        limit: i32,
        hash: i64,
    ) -> Result<UniqueGiftChatThemesResult, String> {
        // Port of `CloudThemes::myGiftThemesLoadMore` (`data_cloud_themes.cpp:598-646`).
        let effective_limit = if limit <= 0 {
            UNIQUE_GIFT_THEMES_DEFAULT_LIMIT
        } else {
            limit
        };
        let resp = invoke_rpc(
            self,
            tl_fns::account::GetUniqueGiftChatThemes {
                offset: offset.to_owned(),
                limit: effective_limit,
                hash,
            },
        )?;
        match resp {
            tl_enums::account::ChatThemes::NotModified => {
                Ok(UniqueGiftChatThemesResult::NotModified)
            }
            tl_enums::account::ChatThemes::ChatThemes(data) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &data.users, &data.chats);
                });
                let parsed: Vec<UniqueGiftChatThemeRecord> = data
                    .themes
                    .iter()
                    .filter_map(|t| match t {
                        tl_enums::ChatTheme::UniqueGift(u) => unique_gift_chat_theme_from_tl(u),
                        tl_enums::ChatTheme::ChatTheme(_) => None,
                    })
                    .collect();
                let next_offset = data.next_offset.clone();
                with_themes_cache(self, |c| {
                    if offset.is_empty() || c.unique_gift_themes.is_empty() {
                        c.unique_gift_themes_hash = data.hash;
                        c.unique_gift_themes = parsed.clone();
                    } else {
                        for item in &parsed {
                            if !c
                                .unique_gift_themes
                                .iter()
                                .any(|existing| existing.gift_id == item.gift_id)
                            {
                                c.unique_gift_themes.push(item.clone());
                            }
                        }
                    }
                    c.unique_gift_themes_next_offset = next_offset.clone();
                });
                self.bridge_state().notify_ui();
                Ok(UniqueGiftChatThemesResult::Themes {
                    hash: data.hash,
                    themes: parsed,
                    next_offset,
                })
            }
        }
    }

    fn get_share_theme_backdrops(&self, hash: i32) -> Result<ShareThemeBackdropsResult, String> {
        // Port of `Api::ShareThemes::requestBackdrops` (`api_share_themes.cpp:190-231`).
        let resp = invoke_rpc(self, tl_fns::messages::GetShareThemeBackdrops { hash })?;
        match resp {
            tl_enums::ShareThemeBackdrops::NotModified => {
                Ok(ShareThemeBackdropsResult::NotModified)
            }
            tl_enums::ShareThemeBackdrops::ShareThemeBackdrops(data) => {
                let backdrops: Vec<ShareThemeBackdropOption> = data
                    .backdrops
                    .iter()
                    .map(share_theme_backdrop_from_tl)
                    .collect();
                with_themes_cache(self, |c| {
                    c.share_theme_backdrops_hash = data.hash;
                    c.share_theme_backdrops = backdrops.clone();
                });
                self.bridge_state().notify_ui();
                Ok(ShareThemeBackdropsResult::Backdrops {
                    hash: data.hash,
                    backdrops,
                })
            }
        }
    }

    fn set_share_theme(&self, peer_id: i64, theme: &ShareThemeChoice) -> Result<(), String> {
        // Port of `Api::ShareThemes::save` (`api_share_themes.cpp:59-84, 148-179`).
        let peer = resolve_input_peer(self, peer_id)?;
        let tl_theme = share_theme_choice_to_tl(self, theme)?;
        let _ = invoke_rpc(
            self,
            tl_fns::messages::SetShareTheme {
                peer,
                theme: tl_theme,
            },
        )?;
        with_themes_cache(self, |c| {
            c.peer_share_themes.insert(peer_id, theme.clone());
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn get_peer_colors(&self, hash: i32) -> Result<PeerColorsResult, String> {
        // Port of `Api::PeerColors::request` & `apply` (`api_peer_colors.cpp:33-49, 126-193`),
        // synced with `tg_api::cached_lists::PeerColorsSync`.
        let resp = invoke_rpc(self, tl_fns::help::GetPeerColors { hash })?;
        match resp {
            tl_enums::help::PeerColors::NotModified => Ok(PeerColorsResult::NotModified),
            tl_enums::help::PeerColors::PeerColors(data) => {
                let (options, suggested) = parse_peer_colors_from_tl(&data.colors);
                let sync_entries: Vec<CachedPeerColorOption> = options
                    .iter()
                    .map(|o| CachedPeerColorOption {
                        color_id: i32::from(o.color_id),
                        hidden: o.hidden,
                        channel_min_level: o.channel_min_level,
                        group_min_level: o.group_min_level,
                    })
                    .collect();
                with_themes_cache(self, |c| {
                    c.peer_colors_sync.hash = data.hash;
                    c.peer_colors_sync.colors = sync_entries;
                    c.peer_colors = options.clone();
                    c.peer_colors_suggested = suggested.clone();
                });
                crate::peer_accent::apply_color_sets(&options);
                self.bridge_state().notify_ui();
                Ok(PeerColorsResult::Colors {
                    hash: data.hash,
                    options,
                    suggested,
                })
            }
        }
    }

    fn get_peer_profile_colors(&self, hash: i32) -> Result<PeerProfileColorsResult, String> {
        // Port of `Api::PeerColors::requestProfile` & `applyProfile`
        // (`api_peer_colors.cpp:51-67, 195-245`), synced with `tg_api::cached_lists::PeerColorsSync`.
        let resp = invoke_rpc(self, tl_fns::help::GetPeerProfileColors { hash })?;
        match resp {
            tl_enums::help::PeerColors::NotModified => Ok(PeerProfileColorsResult::NotModified),
            tl_enums::help::PeerColors::PeerColors(data) => {
                let options = parse_peer_profile_colors_from_tl(&data.colors);
                let sync_entries: Vec<CachedPeerColorOption> = options
                    .iter()
                    .map(|o| CachedPeerColorOption {
                        color_id: i32::from(o.color_id),
                        hidden: o.hidden,
                        channel_min_level: o.channel_min_level,
                        group_min_level: o.group_min_level,
                    })
                    .collect();
                with_themes_cache(self, |c| {
                    c.peer_colors_sync.profile_hash = data.hash;
                    c.peer_colors_sync.profile_colors = sync_entries;
                    c.peer_profile_colors = options.clone();
                });
                self.bridge_state().notify_ui();
                Ok(PeerProfileColorsResult::Colors {
                    hash: data.hash,
                    options,
                })
            }
        }
    }
}

/// Converts a TL [`tl_types::ChatThemeUniqueGift`] into [`UniqueGiftChatThemeRecord`]
/// matching `CloudTheme::Parse(MTPDchatThemeUniqueGift)` (`data_cloud_themes.cpp:155-230`).
#[must_use]
pub fn unique_gift_chat_theme_from_tl(
    data: &tl_types::ChatThemeUniqueGift,
) -> Option<UniqueGiftChatThemeRecord> {
    let tl_enums::StarGift::Unique(ref unique) = data.gift else {
        return None;
    };
    let mut settings_map: BTreeMap<CloudThemeType, CloudThemeSettings> = BTreeMap::new();
    for item in &data.theme_settings {
        let tl_enums::ThemeSettings::ThemeSettings(ts) = item;
        let variant = base_theme_to_cloud_type(&ts.base_theme);
        let is_dark = variant == CloudThemeType::Dark;
        let paper = ts
            .wallpaper
            .as_ref()
            .map(|w| wallpaper_from_tl(w).wallpaper);
        let accent_color = deserialize_tl_color(ts.accent_color);
        let outgoing_messages_colors = ts
            .message_colors
            .as_ref()
            .map(|cols| cols.iter().copied().map(deserialize_tl_color).collect())
            .unwrap_or_default();
        // Exact port of `data_cloud_themes.cpp:201-209`: in light mode with neither
        // explicit `outbox_accent_color` nor `message_colors`, fallback to `accent_color`.
        let outgoing_accent_color = match ts.outbox_accent_color {
            Some(c) => Some(deserialize_tl_color(c)),
            None if !is_dark && ts.message_colors.is_none() => Some(accent_color),
            None => None,
        };
        settings_map.insert(
            variant,
            CloudThemeSettings {
                paper,
                accent_color,
                outgoing_accent_color,
                outgoing_messages_colors,
            },
        );
    }
    Some(UniqueGiftChatThemeRecord {
        gift_id: unique.id as u64,
        slug: unique.slug.clone(),
        title: unique.title.clone(),
        num: unique.num,
        theme: CloudTheme {
            id: unique.id as u64,
            access_hash: 0,
            slug: unique.slug.clone(),
            title: unique.title.clone(),
            document_id: 0,
            created_by: 0,
            users_count: 0,
            emoticon: String::new(),
            settings: settings_map,
        },
    })
}

/// Converts a TL [`tl_enums::ShareThemeBackdropOption`] into [`ShareThemeBackdropOption`]
/// (`api_share_themes.cpp:210-230`).
#[must_use]
pub fn share_theme_backdrop_from_tl(
    opt: &tl_enums::ShareThemeBackdropOption,
) -> ShareThemeBackdropOption {
    let tl_enums::ShareThemeBackdropOption::ShareThemeBackdropOption(d) = opt;
    ShareThemeBackdropOption {
        backdrop_id: d.backdrop_id,
        name: d.name.clone(),
        center_color: deserialize_tl_color(d.center_color),
        edge_color: deserialize_tl_color(d.edge_color),
        pattern_color: deserialize_tl_color(d.pattern_color),
        text_color: deserialize_tl_color(d.text_color),
    }
}

/// Serializes [`ShareThemeChoice`] into TL [`tl_enums::ShareTheme`] matching `Serialize` in
/// `api_share_themes.cpp:59-84`.
fn share_theme_choice_to_tl(
    live: &LiveSession,
    theme: &ShareThemeChoice,
) -> Result<tl_enums::ShareTheme, String> {
    match theme {
        ShareThemeChoice::Default => Ok(tl_enums::ShareTheme::Default),
        ShareThemeChoice::EmojiBackdrop {
            backdrop_id,
            emojis,
        } => {
            if backdrop_id.is_none() && emojis.is_empty() {
                // Exact port of `api_share_themes.cpp:69-71`: empty backdrop+emojis -> default.
                return Ok(tl_enums::ShareTheme::Default);
            }
            let tl_emojis: Vec<tl_enums::Reaction> = emojis
                .iter()
                .map(|r| match r {
                    ShareThemeReaction::Emoji(em) => {
                        tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                            emoticon: em.clone(),
                        })
                    }
                    ShareThemeReaction::CustomEmoji(doc_id) => {
                        tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
                            document_id: *doc_id,
                        })
                    }
                })
                .collect();
            Ok(tl_enums::ShareTheme::EmojiBackdrop(
                tl_types::ShareThemeEmojiBackdrop {
                    backdrop_id: *backdrop_id,
                    emojis: (!tl_emojis.is_empty()).then_some(tl_emojis),
                },
            ))
        }
        ShareThemeChoice::Gift {
            msg_id,
            peer_id,
            saved_id,
            slug,
        } => {
            let stargift = if *msg_id != 0 {
                tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser {
                    msg_id: *msg_id,
                })
            } else if let Some(pid) = peer_id {
                let peer = resolve_input_peer(live, *pid)?;
                tl_enums::InputSavedStarGift::Chat(tl_types::InputSavedStarGiftChat {
                    peer,
                    saved_id: *saved_id,
                })
            } else if let Some(s) = slug.as_deref().filter(|s| !s.is_empty()) {
                tl_enums::InputSavedStarGift::Slug(tl_types::InputSavedStarGiftSlug {
                    slug: s.to_owned(),
                })
            } else {
                return Err(
                    "ShareThemeChoice::Gift requires msg_id, (peer_id, saved_id), or slug."
                        .to_owned(),
                );
            };
            Ok(tl_enums::ShareTheme::InputShareThemeGift(
                tl_types::InputShareThemeGift { stargift },
            ))
        }
    }
}

fn parse_accent_color_set(set: &tl_enums::help::PeerColorSet) -> Vec<Color> {
    match set {
        tl_enums::help::PeerColorSet::PeerColorSet(d) => {
            if d.colors.is_empty() || d.colors.len() > PEER_COLOR_PATTERNS_COUNT {
                return Vec::new();
            }
            d.colors.iter().copied().map(deserialize_tl_color).collect()
        }
        tl_enums::help::PeerColorSet::PeerColorProfileSet(_) => Vec::new(),
    }
}

fn parse_profile_color_set(set: &tl_enums::help::PeerColorSet) -> PeerProfileColorSet {
    match set {
        tl_enums::help::PeerColorSet::PeerColorProfileSet(d) => PeerProfileColorSet {
            palette: d
                .palette_colors
                .iter()
                .copied()
                .map(deserialize_tl_color)
                .collect(),
            bg: d
                .bg_colors
                .iter()
                .copied()
                .map(deserialize_tl_color)
                .collect(),
            story: d
                .story_colors
                .iter()
                .copied()
                .map(deserialize_tl_color)
                .collect(),
        },
        tl_enums::help::PeerColorSet::PeerColorSet(_) => PeerProfileColorSet::default(),
    }
}

/// Parses `help.peerColors.colors` for accent peer colors (`api_peer_colors.cpp:126-193`).
#[must_use]
pub fn parse_peer_colors_from_tl(
    list: &[tl_enums::help::PeerColorOption],
) -> (Vec<PeerColorOptionRecord>, Vec<u8>) {
    let mut options = Vec::with_capacity(list.len());
    let mut suggested = Vec::with_capacity(list.len());
    for opt in list {
        let tl_enums::help::PeerColorOption::PeerColorOption(d) = opt;
        if d.color_id < 0 || d.color_id >= PEER_COLOR_INDEX_COUNT {
            continue;
        }
        let color_id = d.color_id as u8;
        if !d.hidden {
            suggested.push(color_id);
        }
        let light_colors = d
            .colors
            .as_ref()
            .map(parse_accent_color_set)
            .unwrap_or_default();
        let dark_colors = d
            .dark_colors
            .as_ref()
            .map(parse_accent_color_set)
            .unwrap_or_else(|| light_colors.clone());
        options.push(PeerColorOptionRecord {
            color_id,
            hidden: d.hidden,
            channel_min_level: d.channel_min_level,
            group_min_level: d.group_min_level,
            light_colors,
            dark_colors,
        });
    }
    (options, suggested)
}

/// Parses `help.peerColors.colors` for profile colors (`api_peer_colors.cpp:195-245`).
#[must_use]
pub fn parse_peer_profile_colors_from_tl(
    list: &[tl_enums::help::PeerColorOption],
) -> Vec<PeerProfileColorOptionRecord> {
    let mut options = Vec::with_capacity(list.len());
    for opt in list {
        let tl_enums::help::PeerColorOption::PeerColorOption(d) = opt;
        if d.color_id < 0 || d.color_id >= PEER_COLOR_INDEX_COUNT {
            continue;
        }
        let light = d
            .colors
            .as_ref()
            .map(parse_profile_color_set)
            .unwrap_or_default();
        let dark = d
            .dark_colors
            .as_ref()
            .map(parse_profile_color_set)
            .unwrap_or_default();
        options.push(PeerProfileColorOptionRecord {
            color_id: d.color_id as u8,
            hidden: d.hidden,
            channel_min_level: d.channel_min_level,
            group_min_level: d.group_min_level,
            light,
            dark,
        });
    }
    options
}

impl LiveSession {
    /// Returns a shared [`ThemesBackend`] handle pointing to this [`LiveSession`],
    /// ready to plug into [`crate::themes::ThemesStudio::set_backend`] or
    /// [`crate::themes::AppThemesHost::set_backend`].
    #[must_use]
    pub fn themes_backend(&self) -> Arc<dyn ThemesBackend> {
        Arc::new(self.clone())
    }

    /// Saves (`unsave = false`) or removes (`unsave = true`) a cloud theme by share slug
    /// (`account.saveTheme` with `inputThemeSlug`).
    pub fn save_theme_by_slug(&self, slug: &str, unsave: bool) -> Result<(), String> {
        let clean = slug.trim();
        if clean.is_empty() {
            return Err("Theme slug cannot be empty.".to_owned());
        }
        let _ = invoke_rpc(
            self,
            tl_fns::account::SaveTheme {
                theme: tl_enums::InputTheme::Slug(tl_types::InputThemeSlug {
                    slug: clean.to_owned(),
                }),
                unsave,
            },
        )?;
        if unsave {
            with_themes_cache(self, |c| {
                c.themes.retain(|t| t.theme.slug != clean);
            });
        }
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Computes the minimum channel/group boost level required for `color_index`, matching
    /// `Api::PeerColors::requiredLevelFor` (`api_peer_colors.cpp:100-124`).
    #[must_use]
    pub fn peer_color_required_level(
        &self,
        peer_id: i64,
        color_index: u8,
        is_megagroup: bool,
        is_profile: bool,
    ) -> i32 {
        let default_index = tg_data::decide_color_index(to_data_peer(PeerId(peer_id)));
        if default_index == color_index {
            return 0;
        }
        with_themes_cache(self, |c| {
            if is_profile {
                c.peer_profile_colors
                    .iter()
                    .find(|o| o.color_id == color_index)
                    .map_or(1, |opt| {
                        if is_megagroup {
                            opt.group_min_level.unwrap_or(0)
                        } else {
                            opt.channel_min_level.unwrap_or(0)
                        }
                    })
            } else {
                c.peer_colors
                    .iter()
                    .find(|o| o.color_id == color_index)
                    .and_then(|opt| {
                        if is_megagroup {
                            opt.group_min_level
                        } else {
                            opt.channel_min_level
                        }
                    })
                    .unwrap_or(1)
            }
        })
    }

    /// Returns cached cloud themes fetched during this session.
    #[must_use]
    pub fn cached_cloud_themes(&self) -> Vec<CloudThemeRecord> {
        with_themes_cache(self, |c| c.themes.clone())
    }

    /// Returns cached per-chat emoji themes fetched during this session.
    #[must_use]
    pub fn cached_chat_themes(&self) -> Vec<CloudThemeRecord> {
        with_themes_cache(self, |c| c.chat_themes.clone())
    }

    /// Returns cached cloud wallpapers fetched during this session.
    #[must_use]
    pub fn cached_wallpapers(&self) -> Vec<CloudWallPaperRecord> {
        with_themes_cache(self, |c| c.wallpapers.clone())
    }

    /// Returns cached unique Star Gift chat themes fetched during this session.
    #[must_use]
    pub fn cached_unique_gift_chat_themes(&self) -> Vec<UniqueGiftChatThemeRecord> {
        with_themes_cache(self, |c| c.unique_gift_themes.clone())
    }

    /// Returns cached share-theme backdrop options fetched during this session.
    #[must_use]
    pub fn cached_share_theme_backdrops(&self) -> Vec<ShareThemeBackdropOption> {
        with_themes_cache(self, |c| c.share_theme_backdrops.clone())
    }

    /// Returns cached share theme for `peer_id`, if set during this session.
    #[must_use]
    pub fn cached_peer_share_theme(&self, peer_id: i64) -> Option<ShareThemeChoice> {
        with_themes_cache(self, |c| c.peer_share_themes.get(&peer_id).cloned())
    }

    /// Refreshes `help.getPeerColors` if at least [`PEER_COLORS_REFRESH_MS`] (1 hour) has
    /// elapsed since the previous refresh or `force == true`, matching
    /// `tg_api::cached_lists::PeerColorsSync::refresh` (`api_peer_colors.cpp:19`).
    ///
    /// Returns `Ok(true)` if an RPC was sent, `Ok(false)` if throttled.
    pub fn refresh_peer_colors_if_due(&self, now_ms: u64, force: bool) -> Result<bool, String> {
        let (should_run, hash) = with_themes_cache(self, |c| {
            let sync = &c.peer_colors_sync;
            let throttled = !force
                && sync.last_refresh_ms > 0
                && now_ms < sync.last_refresh_ms.saturating_add(PEER_COLORS_REFRESH_MS);
            (!throttled, sync.hash)
        });
        if !should_run {
            return Ok(false);
        }
        let _ = self.get_peer_colors(hash)?;
        with_themes_cache(self, |c| {
            c.peer_colors_sync.last_refresh_ms = now_ms;
        });
        Ok(true)
    }

    /// Refreshes `help.getPeerProfileColors` if at least [`PEER_COLORS_REFRESH_MS`] (1 hour)
    /// has elapsed since the previous refresh or `force == true`, matching
    /// `tg_api::cached_lists::PeerColorsSync::refresh_profile` (`api_peer_colors.cpp:19`).
    ///
    /// Returns `Ok(true)` if an RPC was sent, `Ok(false)` if throttled.
    pub fn refresh_peer_profile_colors_if_due(
        &self,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, String> {
        let (should_run, hash) = with_themes_cache(self, |c| {
            let sync = &c.peer_colors_sync;
            let throttled = !force
                && sync.last_profile_refresh_ms > 0
                && now_ms
                    < sync
                        .last_profile_refresh_ms
                        .saturating_add(PEER_COLORS_REFRESH_MS);
            (!throttled, sync.profile_hash)
        });
        if !should_run {
            return Ok(false);
        }
        let _ = self.get_peer_profile_colors(hash)?;
        with_themes_cache(self, |c| {
            c.peer_colors_sync.last_profile_refresh_ms = now_ms;
        });
        Ok(true)
    }

    /// Returns cached peer accent color options and hash fetched during this session.
    #[must_use]
    pub fn cached_peer_colors(&self) -> (i32, Vec<PeerColorOptionRecord>, Vec<u8>) {
        with_themes_cache(self, |c| {
            (
                c.peer_colors_sync.hash,
                c.peer_colors.clone(),
                c.peer_colors_suggested.clone(),
            )
        })
    }

    /// Returns cached peer profile color options and hash fetched during this session.
    #[must_use]
    pub fn cached_peer_profile_colors(&self) -> (i32, Vec<PeerProfileColorOptionRecord>) {
        with_themes_cache(self, |c| {
            (
                c.peer_colors_sync.profile_hash,
                c.peer_profile_colors.clone(),
            )
        })
    }
}
