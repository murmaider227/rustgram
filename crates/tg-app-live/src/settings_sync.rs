//! Live server-synced settings, profile photo upload, notification exceptions sync,
//! and `tdata` persistence glue for [`LiveSession`] and [`LiveAppHost`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_auto_download.cpp`
//! - `Telegram/SourceFiles/api/api_global_privacy.cpp`
//! - `Telegram/SourceFiles/api/api_self_destruct.cpp`
//! - `Telegram/SourceFiles/api/api_sensitive_content.cpp`
//! - `Telegram/SourceFiles/api/api_peer_photo.cpp`
//! - `Telegram/SourceFiles/data/notify/data_notify_settings.cpp`
//! - `Telegram/SourceFiles/core/core_settings.cpp`
//! - `Telegram/SourceFiles/main/main_session_settings.cpp`

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, peer_id_from_tl, peer_notify_settings_from_tl};
use tg_core::settings::WindowPosition as CoreWindowPosition;
use tg_core::{CoreSettings, GlobalConfig};
use tg_data::{
    DefaultNotifyScope as DataDefaultNotifyScope, Peer, PeerId as DataPeerId,
    PeerNotifySettings as DataPeerNotifySettings,
};
use tg_session::{
    AccountManager, StoredAutoDownload, StoredAutoDownloadSource, StoredAutoDownloadType,
    StoredSessionSettings,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::{debug, warn};

use crate::convert::{
    from_data_peer, known_peer_ids, notify_scope_to_tl, notify_settings_from_tl,
    notify_settings_to_tl, now_unix_i32, peer_to_settings_ref, to_data_peer,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::notifications::settings::{
    DefaultNotify, NotifySettingsStore, NotifySound, PeerNotifySettingsValue,
};
use tg_app_core::settings::backend::{
    AutoDownloadSettings, AutoDownloadSource, AutoDownloadType, GlobalPrivacySettings,
    MAX_AUTO_DOWNLOAD_BYTES, NotifyException, NotifyScope, ProfilePhotoUpload, ScopeNotifySettings,
};
use tg_app_core::stand_in::settings::WindowPosition as ShellWindowPosition;
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

/// Upload part size used for Telegram file uploads (512 KiB).
pub const UPLOAD_PART_SIZE: usize = 512 * 1024;

/// Threshold above which Telegram requires `upload.saveBigFilePart` (10 MiB).
pub const BIG_FILE_THRESHOLD: usize = 10 * 1024 * 1024;

/// Timeout per uploaded file part RPC.
const UPLOAD_PART_TIMEOUT: Duration = Duration::from_secs(30);

/// Server auto-download preset tier returned by `account.getAutoDownloadSettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoDownloadPreset {
    /// Low bandwidth / conservative preset (`low = true`).
    Low,
    /// Medium / balanced preset (`low = false, high = false`).
    #[default]
    Medium,
    /// High bandwidth preset (`high = true`).
    High,
}

/// All three server auto-download presets from `account.getAutoDownloadSettings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAutoDownloadPresets {
    /// Low-bandwidth preset.
    pub low: AutoDownloadSettings,
    /// Balanced medium preset.
    pub medium: AutoDownloadSettings,
    /// High-bandwidth preset.
    pub high: AutoDownloadSettings,
}

impl ServerAutoDownloadPresets {
    /// Returns the settings for the given preset tier.
    #[must_use]
    pub fn for_preset(&self, preset: AutoDownloadPreset) -> &AutoDownloadSettings {
        match preset {
            AutoDownloadPreset::Low => &self.low,
            AutoDownloadPreset::Medium => &self.medium,
            AutoDownloadPreset::High => &self.high,
        }
    }
}

/// Auxiliary fields returned by `account.getGlobalPrivacySettings` that are not directly
/// edited in the Privacy UI, preserved across `account.setGlobalPrivacySettings` saves.
#[derive(Debug, Clone, Default)]
pub(super) struct GlobalPrivacyExtra {
    pub display_gifts_button: bool,
    pub disallowed_gifts: Option<tl_enums::DisallowedGiftsSettings>,
}

// ============================================================================
// 1. Minimal small / big file uploader (`upload.saveFilePart` / `saveBigFilePart`)
// ============================================================================

/// Uploads `bytes` to Telegram using `upload.saveFilePart` (<= 10 MiB) or
/// `upload.saveBigFilePart` (> 10 MiB) and returns the corresponding [`tl_enums::InputFile`].
pub async fn upload_file_bytes_async<C: RpcClient>(
    rpc: &C,
    file_name: &str,
    bytes: &[u8],
) -> Result<tl_enums::InputFile, String> {
    if bytes.is_empty() {
        return Err("Cannot upload an empty file.".to_owned());
    }
    let clean_name = if file_name.trim().is_empty() {
        "photo.jpg".to_owned()
    } else {
        file_name.trim().to_owned()
    };

    let file_id = tg_crypto::random_i64();
    let total_parts = bytes.len().div_ceil(UPLOAD_PART_SIZE) as i32;
    let is_big = bytes.len() > BIG_FILE_THRESHOLD;

    for (idx, chunk) in bytes.chunks(UPLOAD_PART_SIZE).enumerate() {
        let part_idx = idx as i32;
        if is_big {
            let req = tl_fns::upload::SaveBigFilePart {
                file_id,
                file_part: part_idx,
                file_total_parts: total_parts,
                bytes: chunk.to_vec(),
            };
            let ok = with_rpc_timeout(UPLOAD_PART_TIMEOUT, async {
                rpc.invoke(&req)
                    .await
                    .map_err(Into::<tg_api::rpc::InvocationError>::into)
            })
            .await?;
            if !ok {
                return Err(format!(
                    "Server rejected big upload part {part_idx}/{total_parts}."
                ));
            }
        } else {
            let req = tl_fns::upload::SaveFilePart {
                file_id,
                file_part: part_idx,
                bytes: chunk.to_vec(),
            };
            let ok = with_rpc_timeout(UPLOAD_PART_TIMEOUT, async {
                rpc.invoke(&req)
                    .await
                    .map_err(Into::<tg_api::rpc::InvocationError>::into)
            })
            .await?;
            if !ok {
                return Err(format!(
                    "Server rejected upload part {part_idx}/{total_parts}."
                ));
            }
        }
    }

    if is_big {
        Ok(tl_enums::InputFile::Big(tl_types::InputFileBig {
            id: file_id,
            parts: total_parts,
            name: clean_name,
        }))
    } else {
        Ok(tl_enums::InputFile::InputFile(tl_types::InputFile {
            id: file_id,
            parts: total_parts,
            name: clean_name,
            md5_checksum: tg_crypto::md5_hex(bytes),
        }))
    }
}

/// Synchronously uploads `bytes` over `live`'s Tokio runtime and RPC client.
pub fn upload_file_bytes(
    live: &LiveSession,
    file_name: &str,
    bytes: &[u8],
) -> Result<tl_enums::InputFile, String> {
    let rpc = live.runtime.rpc().clone();
    let name = file_name.to_owned();
    let data = bytes.to_vec();
    block_on_runtime(live.runtime.tokio_handle(), async move {
        upload_file_bytes_async(&rpc, &name, &data).await
    })
}

// ============================================================================
// 2. Profile photo upload / update / clear (`photos.uploadProfilePhoto`)
// ============================================================================

/// Uploads a static profile photo, animated video avatar, or public fallback photo via
/// `photos.uploadProfilePhoto`, applies returned user updates to `tg_data::Session`, and
/// returns the assigned Telegram photo ID.
pub fn upload_profile_photo_live(
    live: &mut LiveSession,
    request: ProfilePhotoUpload,
) -> Result<i64, String> {
    if request.image.is_none() && request.video.is_none() {
        return Err("Profile photo upload requires an image or video payload.".to_owned());
    }

    let file = match &request.image {
        Some((name, bytes)) => Some(upload_file_bytes(live, name, bytes)?),
        None => None,
    };
    let video = match &request.video {
        Some((name, bytes)) => Some(upload_file_bytes(live, name, bytes)?),
        None => None,
    };

    let req = tl_fns::photos::UploadProfilePhoto {
        fallback: request.fallback,
        bot: None,
        file,
        video,
        video_start_ts: request.video_start_ts,
        video_emoji_markup: None,
    };
    let tl_enums::photos::Photo::Photo(resp) = live.invoke_rpc(req)?;

    let photo_id = match &resp.photo {
        tl_enums::Photo::Photo(p) => p.id,
        tl_enums::Photo::Empty(p) => p.id,
    };

    live.write_session(|s| {
        apply_tl_users_and_chats(s, &resp.users, &[]);
    });

    let label = request
        .image
        .as_ref()
        .or(request.video.as_ref())
        .map(|(n, _)| n.clone())
        .or_else(|| request.fallback.then_some("Fallback photo".to_owned()));
    if let Ok(mut c) = live.cache.lock() {
        c.photo_label = label;
    }
    live.refresh_cached_views();
    live.bridge.state().notify_ui();
    Ok(photo_id)
}

/// Implements [`tg_app_core::settings::backend::SettingsBackend::update_profile_photo`] in live mode.
///
/// Always updates the local avatar color/label cache; when `photo_label` is a path to an
/// existing local file on disk, reads and uploads it via `photos.uploadProfilePhoto`
/// (detecting `.mp4`/`.mov`/`.webm` as a video avatar).
pub fn update_profile_photo_live(
    live: &mut LiveSession,
    color_index: u8,
    photo_label: Option<String>,
) {
    if let Ok(mut c) = live.cache.lock() {
        c.photo_color_index = Some(color_index);
        c.photo_label = photo_label.clone();
    }

    let Some(raw_label) = photo_label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let path = Path::new(raw_label);
    if !path.is_file() {
        return;
    }

    let bytes = match std::fs::read(path) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return,
        Err(err) => {
            warn!(path = %path.display(), error = %err, "failed to read profile photo file");
            return;
        }
    };

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("avatar.jpg")
        .to_owned();
    let is_video = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            ext.eq_ignore_ascii_case("mp4")
                || ext.eq_ignore_ascii_case("mov")
                || ext.eq_ignore_ascii_case("webm")
        });

    let request = if is_video {
        ProfilePhotoUpload::video_avatar(file_name, bytes, None, false)
    } else {
        ProfilePhotoUpload::static_photo(file_name, bytes, false)
    };
    if let Err(err) = upload_profile_photo_live(live, request) {
        warn!(error = %err, "photos.uploadProfilePhoto failed");
    }
}

/// Clears the current primary (or public fallback) profile photo via
/// `photos.updateProfilePhoto(id: inputPhotoEmpty)`.
pub fn clear_profile_photo_live(live: &mut LiveSession, fallback: bool) -> Result<(), String> {
    let req = tl_fns::photos::UpdateProfilePhoto {
        fallback,
        bot: None,
        id: tl_enums::InputPhoto::Empty,
    };
    let tl_enums::photos::Photo::Photo(resp) = live.invoke_rpc(req)?;
    live.write_session(|s| {
        apply_tl_users_and_chats(s, &resp.users, &[]);
    });
    if !fallback && let Ok(mut c) = live.cache.lock() {
        c.photo_label = None;
    }
    live.refresh_cached_views();
    live.bridge.state().notify_ui();
    Ok(())
}

// ============================================================================
// 3. Auto-download sync (`account.getAutoDownloadSettings` / `saveAutoDownloadSettings`)
// ============================================================================

/// Converts a TL `AutoDownloadSettings` preset into [`AutoDownloadSettings`],
/// preserving local autoplay toggles `(videos, gifs, round_videos)`.
#[must_use]
pub fn auto_download_from_tl(
    tl: &tl_types::AutoDownloadSettings,
    autoplay: (bool, bool, bool),
) -> AutoDownloadSettings {
    let mut out = AutoDownloadSettings {
        limits: [[0; 7]; 3],
        autoplay_videos: autoplay.0,
        autoplay_gifs: autoplay.1,
        autoplay_round_videos: autoplay.2,
    };
    if tl.disabled {
        return out;
    }

    let photo_cap = i64::from(tl.photo_size_max.max(0)).clamp(0, MAX_AUTO_DOWNLOAD_BYTES);
    let video_cap = tl.video_size_max.clamp(0, MAX_AUTO_DOWNLOAD_BYTES);
    let file_cap = tl.file_size_max.clamp(0, MAX_AUTO_DOWNLOAD_BYTES);

    for src in AutoDownloadSource::ALL {
        out.set_bytes_limit(src, AutoDownloadType::Photo, photo_cap);
        out.set_bytes_limit(src, AutoDownloadType::AutoPlayVideo, video_cap);
        out.set_bytes_limit(src, AutoDownloadType::AutoPlayVideoMessage, video_cap);
        out.set_bytes_limit(src, AutoDownloadType::AutoPlayGif, video_cap);
        out.set_bytes_limit(src, AutoDownloadType::VoiceMessage, file_cap);
        out.set_bytes_limit(src, AutoDownloadType::Music, file_cap);
        out.set_bytes_limit(src, AutoDownloadType::File, file_cap);
    }
    out
}

/// Converts [`AutoDownloadSettings`] into a Layer 233 TL `AutoDownloadSettings` payload.
#[must_use]
pub fn auto_download_to_tl(settings: &AutoDownloadSettings) -> tl_enums::AutoDownloadSettings {
    let disabled = !AutoDownloadSource::ALL
        .iter()
        .any(|&src| settings.has_enabled_types(src));

    let mut photo_max = 0i64;
    let mut video_max = 0i64;
    let mut file_max = 0i64;
    for src in AutoDownloadSource::ALL {
        photo_max = photo_max.max(settings.bytes_limit(src, AutoDownloadType::Photo));
        video_max = video_max
            .max(settings.bytes_limit(src, AutoDownloadType::AutoPlayVideo))
            .max(settings.bytes_limit(src, AutoDownloadType::AutoPlayVideoMessage))
            .max(settings.bytes_limit(src, AutoDownloadType::AutoPlayGif));
        file_max = file_max
            .max(settings.bytes_limit(src, AutoDownloadType::File))
            .max(settings.bytes_limit(src, AutoDownloadType::Music))
            .max(settings.bytes_limit(src, AutoDownloadType::VoiceMessage));
    }

    tl_enums::AutoDownloadSettings::AutoDownloadSettings(tl_types::AutoDownloadSettings {
        disabled,
        video_preload_large: settings.autoplay_videos,
        audio_preload_next: true,
        phonecalls_less_data: false,
        stories_preload: false,
        photo_size_max: photo_max.clamp(0, i64::from(i32::MAX)) as i32,
        video_size_max: video_max.clamp(0, MAX_AUTO_DOWNLOAD_BYTES),
        file_size_max: file_max.clamp(0, MAX_AUTO_DOWNLOAD_BYTES),
        video_upload_maxbitrate: 0,
        small_queue_active_operations_max: 0,
        large_queue_active_operations_max: 0,
    })
}

/// Converts [`AutoDownloadSettings`] into tdesktop's `tdata` [`StoredAutoDownload`] structure.
#[must_use]
pub fn auto_download_to_stored(settings: &AutoDownloadSettings) -> StoredAutoDownload {
    let mut stored = StoredAutoDownload::default();
    for (si, &src) in StoredAutoDownloadSource::ALL.iter().enumerate() {
        let ui_src = AutoDownloadSource::ALL[si];
        for (ti, &kind) in StoredAutoDownloadType::ALL.iter().enumerate() {
            let ui_kind = AutoDownloadType::ALL[ti];
            stored.set_bytes_limit(src, kind, settings.bytes_limit(ui_src, ui_kind));
        }
    }
    stored
}

/// Converts tdesktop's `tdata` [`StoredAutoDownload`] into [`AutoDownloadSettings`].
#[must_use]
pub fn auto_download_from_stored(
    stored: &StoredAutoDownload,
    autoplay: (bool, bool, bool),
) -> AutoDownloadSettings {
    let mut out = AutoDownloadSettings {
        limits: [[0; 7]; 3],
        autoplay_videos: autoplay.0,
        autoplay_gifs: autoplay.1,
        autoplay_round_videos: autoplay.2,
    };
    for (si, &src) in StoredAutoDownloadSource::ALL.iter().enumerate() {
        let ui_src = AutoDownloadSource::ALL[si];
        for (ti, &kind) in StoredAutoDownloadType::ALL.iter().enumerate() {
            let ui_kind = AutoDownloadType::ALL[ti];
            out.set_bytes_limit(ui_src, ui_kind, stored.bytes_limit(src, kind));
        }
    }
    out
}

/// Lazily loads server auto-download presets via `account.getAutoDownloadSettings` and
/// returns the active [`AutoDownloadSettings`].
pub fn fetch_auto_download_live(live: &LiveSession) -> AutoDownloadSettings {
    // While one request is in flight (the startup warm-up sends it), other callers get the
    // cached values instead of a second request that would block the UI thread until the
    // connection is ready; the window repaints when the reply is applied.
    let (loaded, cached, active_preset, from_tdata) = live
        .cache
        .lock()
        .map(|mut c| {
            let skip = c.auto_download_loaded || c.auto_download_fetching;
            if !skip {
                c.auto_download_fetching = true;
            }
            (
                skip,
                c.auto_download.clone(),
                c.active_auto_download_preset,
                c.auto_download_from_tdata,
            )
        })
        .unwrap_or((
            true,
            AutoDownloadSettings::default(),
            AutoDownloadPreset::Medium,
            false,
        ));
    if loaded {
        return cached;
    }

    let autoplay = (
        cached.autoplay_videos,
        cached.autoplay_gifs,
        cached.autoplay_round_videos,
    );

    match live.invoke_rpc(tl_fns::account::GetAutoDownloadSettings {}) {
        Ok(tl_enums::account::AutoDownloadSettings::AutoDownloadSettings(resp)) => {
            let tl_enums::AutoDownloadSettings::AutoDownloadSettings(low_tl) = resp.low;
            let tl_enums::AutoDownloadSettings::AutoDownloadSettings(med_tl) = resp.medium;
            let tl_enums::AutoDownloadSettings::AutoDownloadSettings(high_tl) = resp.high;
            let presets = ServerAutoDownloadPresets {
                low: auto_download_from_tl(&low_tl, autoplay),
                medium: auto_download_from_tl(&med_tl, autoplay),
                high: auto_download_from_tl(&high_tl, autoplay),
            };
            let effective = if from_tdata {
                cached
            } else {
                presets.for_preset(active_preset).clone()
            };
            if let Ok(mut c) = live.cache.lock() {
                c.auto_download_loaded = true;
                c.auto_download_fetching = false;
                c.auto_download_presets = Some(presets);
                c.auto_download = effective.clone();
            }
            effective
        }
        Err(err) => {
            debug!(error = %err, "account.getAutoDownloadSettings failed");
            if let Ok(mut c) = live.cache.lock() {
                c.auto_download_loaded = true;
                c.auto_download_fetching = false;
            }
            cached
        }
    }
}

/// Returns the cached server auto-download presets (`low`, `medium`, `high`), fetching
/// them via `account.getAutoDownloadSettings` if not yet loaded.
pub fn fetch_auto_download_presets_live(live: &LiveSession) -> Option<ServerAutoDownloadPresets> {
    let _ = fetch_auto_download_live(live);
    live.cache
        .lock()
        .ok()
        .and_then(|c| c.auto_download_presets.clone())
}

/// Saves `settings` to Telegram via `account.saveAutoDownloadSettings` and marks
/// per-account `SessionSettings` dirty for `tdata` persistence.
pub fn save_auto_download_live(live: &mut LiveSession, settings: AutoDownloadSettings) {
    let preset = live
        .cache
        .lock()
        .map(|c| c.active_auto_download_preset)
        .unwrap_or_default();
    let req = tl_fns::account::SaveAutoDownloadSettings {
        low: preset == AutoDownloadPreset::Low,
        high: preset == AutoDownloadPreset::High,
        settings: auto_download_to_tl(&settings),
    };
    if let Err(err) = live.invoke_rpc(req) {
        debug!(error = %err, "account.saveAutoDownloadSettings failed");
    }
    if let Ok(mut c) = live.cache.lock() {
        c.auto_download_loaded = true;
        c.auto_download_from_tdata = true;
        c.auto_download = settings;
        c.session_settings_dirty = true;
    }
    live.bridge.state().notify_ui();
}

/// Switches to one of the server auto-download presets (`Low`, `Medium`, `High`),
/// persists it via `account.saveAutoDownloadSettings`, and updates the live cache.
pub fn apply_auto_download_preset_live(
    live: &mut LiveSession,
    preset: AutoDownloadPreset,
) -> Result<AutoDownloadSettings, String> {
    let presets = fetch_auto_download_presets_live(live)
        .ok_or_else(|| "Auto-download presets are unavailable.".to_owned())?;
    let mut chosen = presets.for_preset(preset).clone();
    if let Ok(c) = live.cache.lock() {
        chosen.autoplay_videos = c.auto_download.autoplay_videos;
        chosen.autoplay_gifs = c.auto_download.autoplay_gifs;
        chosen.autoplay_round_videos = c.auto_download.autoplay_round_videos;
    }
    let req = tl_fns::account::SaveAutoDownloadSettings {
        low: preset == AutoDownloadPreset::Low,
        high: preset == AutoDownloadPreset::High,
        settings: auto_download_to_tl(&chosen),
    };
    let _ = live.invoke_rpc(req)?;
    if let Ok(mut c) = live.cache.lock() {
        c.active_auto_download_preset = preset;
        c.auto_download_loaded = true;
        c.auto_download_from_tdata = true;
        c.auto_download = chosen.clone();
        c.session_settings_dirty = true;
    }
    live.bridge.state().notify_ui();
    Ok(chosen)
}

// ============================================================================
// 4. Global privacy, default auto-delete TTL, and sensitive content
// ============================================================================

/// Fetches the global default auto-delete TTL (`messages.getDefaultHistoryTTL`).
pub fn fetch_default_history_ttl_live(live: &LiveSession) -> Result<i32, String> {
    let tl_enums::DefaultHistoryTTL::DefaultHistoryTTL(resp) =
        live.invoke_rpc(tl_fns::messages::GetDefaultHistoryTTL {})?;
    let period = resp.period.max(0);
    if let Ok(mut c) = live.cache.lock() {
        c.global_privacy.global_ttl_seconds = period;
    }
    Ok(period)
}

/// Updates the global default auto-delete TTL (`messages.setDefaultHistoryTTL`).
pub fn set_default_history_ttl_live(
    live: &mut LiveSession,
    period_seconds: i32,
) -> Result<(), String> {
    let period = period_seconds.max(0);
    let _ = live.invoke_rpc(tl_fns::messages::SetDefaultHistoryTTL { period })?;
    if let Ok(mut c) = live.cache.lock() {
        c.global_privacy.global_ttl_seconds = period;
    }
    live.bridge.state().notify_ui();
    Ok(())
}

/// Fetches sensitive content settings (`account.getContentSettings`), returning
/// `(sensitive_enabled, sensitive_can_change)`.
pub fn fetch_content_settings_live(live: &LiveSession) -> Result<(bool, bool), String> {
    let tl_enums::account::ContentSettings::ContentSettings(resp) =
        live.invoke_rpc(tl_fns::account::GetContentSettings {})?;
    if let Ok(mut c) = live.cache.lock() {
        c.global_privacy.disable_sensitive_content_filtering = resp.sensitive_enabled;
        c.global_privacy.sensitive_can_change = resp.sensitive_can_change;
    }
    Ok((resp.sensitive_enabled, resp.sensitive_can_change))
}

/// Updates sensitive content filtering (`account.setContentSettings`).
pub fn set_content_settings_live(
    live: &mut LiveSession,
    sensitive_enabled: bool,
) -> Result<(), String> {
    let can_change = live
        .cache
        .lock()
        .map(|c| !c.global_privacy_loaded || c.global_privacy.sensitive_can_change)
        .unwrap_or(true);
    if !can_change {
        return Err("Sensitive content filtering cannot be changed on this account.".to_owned());
    }
    let _ = live.invoke_rpc(tl_fns::account::SetContentSettings { sensitive_enabled })?;
    if let Ok(mut c) = live.cache.lock() {
        c.global_privacy.disable_sensitive_content_filtering = sensitive_enabled;
    }
    live.bridge.state().notify_ui();
    Ok(())
}

/// Fetches combined global privacy (`account.getGlobalPrivacySettings`), default auto-delete
/// TTL (`messages.getDefaultHistoryTTL`), and sensitive content (`account.getContentSettings`).
pub fn fetch_global_privacy_live(live: &LiveSession) -> GlobalPrivacySettings {
    let cached = live
        .cache
        .lock()
        .ok()
        .filter(|c| c.global_privacy_loaded)
        .map(|c| c.global_privacy.clone());
    if let Some(gp) = cached {
        return gp;
    }

    let mut out = GlobalPrivacySettings::default();
    let mut extra = GlobalPrivacyExtra::default();

    match live.invoke_rpc(tl_fns::account::GetGlobalPrivacySettings {}) {
        Ok(tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(resp)) => {
            out.archive_and_mute_new = resp.archive_and_mute_new_noncontact_peers;
            out.keep_archived_unmuted = resp.keep_archived_unmuted;
            out.keep_archived_folders = resp.keep_archived_folders;
            out.hide_read_time = resp.hide_read_marks;
            out.new_noncontact_peers_require_premium = resp.new_noncontact_peers_require_premium;
            out.noncontact_peers_paid_stars = resp.noncontact_peers_paid_stars;
            extra.display_gifts_button = resp.display_gifts_button;
            extra.disallowed_gifts = resp.disallowed_gifts;
        }
        Err(err) => {
            debug!(error = %err, "account.getGlobalPrivacySettings failed");
        }
    }

    match live.invoke_rpc(tl_fns::messages::GetDefaultHistoryTTL {}) {
        Ok(tl_enums::DefaultHistoryTTL::DefaultHistoryTTL(resp)) => {
            out.global_ttl_seconds = resp.period.max(0);
        }
        Err(err) => {
            debug!(error = %err, "messages.getDefaultHistoryTTL failed");
        }
    }

    match live.invoke_rpc(tl_fns::account::GetContentSettings {}) {
        Ok(tl_enums::account::ContentSettings::ContentSettings(resp)) => {
            out.disable_sensitive_content_filtering = resp.sensitive_enabled;
            out.sensitive_can_change = resp.sensitive_can_change;
        }
        Err(err) => {
            debug!(error = %err, "account.getContentSettings failed");
        }
    }

    if let Ok(mut c) = live.cache.lock() {
        c.global_privacy_loaded = true;
        c.global_privacy = out.clone();
        c.global_privacy_extra = Some(extra);
    }
    out
}

/// Saves changed global privacy fields across `account.setGlobalPrivacySettings`,
/// `messages.setDefaultHistoryTTL`, and `account.setContentSettings`.
pub fn save_global_privacy_live(live: &mut LiveSession, settings: GlobalPrivacySettings) {
    let (was_loaded, prev, extra) = live
        .cache
        .lock()
        .map(|c| {
            (
                c.global_privacy_loaded,
                c.global_privacy.clone(),
                c.global_privacy_extra.clone().unwrap_or_default(),
            )
        })
        .unwrap_or((
            false,
            GlobalPrivacySettings::default(),
            GlobalPrivacyExtra::default(),
        ));

    let gp_changed = !was_loaded
        || settings.archive_and_mute_new != prev.archive_and_mute_new
        || settings.keep_archived_unmuted != prev.keep_archived_unmuted
        || settings.keep_archived_folders != prev.keep_archived_folders
        || settings.hide_read_time != prev.hide_read_time
        || settings.new_noncontact_peers_require_premium
            != prev.new_noncontact_peers_require_premium
        || settings.noncontact_peers_paid_stars != prev.noncontact_peers_paid_stars;

    let ttl_changed = (!was_loaded && settings.global_ttl_seconds > 0)
        || (was_loaded && settings.global_ttl_seconds != prev.global_ttl_seconds);

    let sensitive_changed = (!was_loaded && settings.disable_sensitive_content_filtering)
        || (was_loaded
            && settings.disable_sensitive_content_filtering
                != prev.disable_sensitive_content_filtering);

    let mut effective = if was_loaded {
        prev.clone()
    } else {
        settings.clone()
    };

    if gp_changed {
        // Use `tg_api::account::AccountSettingsManager::set_global_privacy_settings`
        // (`api/api_global_privacy.cpp:217-296`): always sends `noncontact_peers_paid_stars`
        // (even when 0) and `disallowed_gifts`, and retries without premium-only fields on
        // `PREMIUM_ACCOUNT_REQUIRED`.
        let unarchive_on_new_message = if settings.keep_archived_unmuted {
            tg_api::account::UnarchiveOnNewMessage::None
        } else if settings.keep_archived_folders {
            tg_api::account::UnarchiveOnNewMessage::NotInFoldersUnmuted
        } else {
            tg_api::account::UnarchiveOnNewMessage::AnyUnmuted
        };
        let mut disallowed_gifts = tg_api::account::DisallowedGifts {
            display_gifts_button: extra.display_gifts_button,
            ..tg_api::account::DisallowedGifts::default()
        };
        if let Some(tl_enums::DisallowedGiftsSettings::DisallowedGiftsSettings(g)) =
            &extra.disallowed_gifts
        {
            disallowed_gifts.unlimited = g.disallow_unlimited_stargifts;
            disallowed_gifts.limited = g.disallow_limited_stargifts;
            disallowed_gifts.unique = g.disallow_unique_stargifts;
            disallowed_gifts.premium = g.disallow_premium_gifts;
            disallowed_gifts.from_channels = g.disallow_stargifts_from_channels;
        }
        let gp_state = tg_api::account::GlobalPrivacyState {
            archive_and_mute_new_noncontact_peers: settings.archive_and_mute_new,
            unarchive_on_new_message,
            hide_read_marks: settings.hide_read_time,
            new_noncontact_peers_require_premium: settings.new_noncontact_peers_require_premium,
            noncontact_peers_paid_stars: settings.noncontact_peers_paid_stars.unwrap_or(0).max(0),
            disallowed_gifts,
        };
        let rpc = live.runtime().rpc().clone();
        let res = block_on_runtime(live.runtime().tokio_handle(), async move {
            tokio::time::timeout(Duration::from_secs(10), async move {
                let mut mgr = tg_api::account::AccountSettingsManager::new();
                mgr.set_global_privacy_settings(&rpc, gp_state, true).await
            })
            .await
        });
        match res {
            Ok(Ok(resp)) => {
                effective.archive_and_mute_new = resp.archive_and_mute_new_noncontact_peers;
                effective.keep_archived_unmuted =
                    resp.unarchive_on_new_message == tg_api::account::UnarchiveOnNewMessage::None;
                effective.keep_archived_folders = resp.unarchive_on_new_message
                    != tg_api::account::UnarchiveOnNewMessage::AnyUnmuted;
                effective.hide_read_time = resp.hide_read_marks;
                effective.new_noncontact_peers_require_premium =
                    resp.new_noncontact_peers_require_premium;
                effective.noncontact_peers_paid_stars = (resp.noncontact_peers_paid_stars > 0)
                    .then_some(resp.noncontact_peers_paid_stars);
                let dg = resp.disallowed_gifts;
                let has_dg =
                    dg.unlimited || dg.limited || dg.unique || dg.premium || dg.from_channels;
                if let Ok(mut c) = live.cache.lock() {
                    c.global_privacy_extra = Some(GlobalPrivacyExtra {
                        display_gifts_button: dg.display_gifts_button,
                        disallowed_gifts: has_dg.then_some(
                            tl_enums::DisallowedGiftsSettings::DisallowedGiftsSettings(
                                tl_types::DisallowedGiftsSettings {
                                    disallow_unlimited_stargifts: dg.unlimited,
                                    disallow_limited_stargifts: dg.limited,
                                    disallow_unique_stargifts: dg.unique,
                                    disallow_premium_gifts: dg.premium,
                                    disallow_stargifts_from_channels: dg.from_channels,
                                },
                            ),
                        ),
                    });
                }
            }
            Ok(Err(err)) => {
                warn!(error = %err, "account.setGlobalPrivacySettings failed");
            }
            Err(_) => {
                warn!("account.setGlobalPrivacySettings timed out");
            }
        }
    }

    if ttl_changed {
        let period = settings.global_ttl_seconds.max(0);
        match live.invoke_rpc(tl_fns::messages::SetDefaultHistoryTTL { period }) {
            Ok(_) => {
                effective.global_ttl_seconds = period;
            }
            Err(err) => {
                warn!(period, error = %err, "messages.setDefaultHistoryTTL failed");
            }
        }
    }

    if sensitive_changed {
        if was_loaded && !prev.sensitive_can_change {
            effective.disable_sensitive_content_filtering =
                prev.disable_sensitive_content_filtering;
            effective.sensitive_can_change = false;
        } else {
            let enabled = settings.disable_sensitive_content_filtering;
            match live.invoke_rpc(tl_fns::account::SetContentSettings {
                sensitive_enabled: enabled,
            }) {
                Ok(_) => {
                    effective.disable_sensitive_content_filtering = enabled;
                }
                Err(err) => {
                    warn!(enabled, error = %err, "account.setContentSettings failed");
                }
            }
        }
    }

    if let Ok(mut c) = live.cache.lock() {
        c.global_privacy_loaded = true;
        c.global_privacy = effective;
    }
    live.bridge.state().notify_ui();
}

// ============================================================================
// 5. Notification exceptions sync (`account.getNotifyExceptions` <-> `NotifySettingsStore`)
// ============================================================================

/// Maps [`NotifyScope`] to [`DefaultNotify`].
#[must_use]
pub const fn notify_scope_to_default_notify(scope: NotifyScope) -> DefaultNotify {
    match scope {
        NotifyScope::PrivateChats => DefaultNotify::User,
        NotifyScope::Groups => DefaultNotify::Group,
        NotifyScope::Channels => DefaultNotify::Broadcast,
    }
}

/// Returns `true` if `peer` belongs to the given notification scope (`DefaultNotifyType`).
#[must_use]
pub fn peer_matches_notify_scope(peer: &Peer, scope: NotifyScope) -> bool {
    match (peer, scope) {
        (Peer::User(_), NotifyScope::PrivateChats) => true,
        (Peer::Chat(_), NotifyScope::Groups) => true,
        (Peer::Channel(ch), NotifyScope::Groups) => !ch.is_broadcast(),
        (Peer::Channel(ch), NotifyScope::Channels) => ch.is_broadcast(),
        _ => false,
    }
}

fn data_peer_kind(peer: &Peer) -> PeerKind {
    match peer {
        Peer::User(_) => PeerKind::User,
        Peer::Chat(_) => PeerKind::Group,
        Peer::Channel(ch) => {
            if ch.is_broadcast() {
                PeerKind::Channel
            } else {
                PeerKind::Group
            }
        }
    }
}

fn sound_name_to_notify_sound(name: &str) -> NotifySound {
    match crate::convert::sound_name_to_api_sound(name) {
        tg_api::account::NotifySound::None => NotifySound::silent(),
        tg_api::account::NotifySound::Default => NotifySound::default_sound(),
        tg_api::account::NotifySound::Ringtone(id) if id >= 0 => NotifySound::ringtone(id as u64),
        tg_api::account::NotifySound::Ringtone(_) => NotifySound::default_sound(),
        tg_api::account::NotifySound::Local { title, data } => NotifySound::local(title, data),
    }
}

fn sound_name_to_tl(name: &str) -> tl_enums::NotificationSound {
    crate::convert::sound_name_to_api_sound(name).to_tl()
}

fn apply_notify_exceptions_updates(live: &LiveSession, updates: tl_enums::Updates, now_secs: i32) {
    let _ = live.apply_updates_to_session(&updates);
    let update_list: &[tl_enums::Update] = match &updates {
        tl_enums::Updates::Updates(u) => &u.updates,
        tl_enums::Updates::Combined(c) => &c.updates,
        tl_enums::Updates::UpdateShort(s) => std::slice::from_ref(&s.update),
        _ => return,
    };

    let mut seen_peers: Vec<DataPeerId> = Vec::new();
    live.write_session(|session| {
        for upd in update_list {
            if let tl_enums::Update::NotifySettings(ns) = upd
                && let tl_enums::NotifyPeer::NotifyPeer(np) = &ns.peer
            {
                let peer_id = peer_id_from_tl(&np.peer);
                let parsed = peer_notify_settings_from_tl(&ns.notify_settings);
                // Match `NotifySettings::isMuted` (`data/notify/data_notify_settings.cpp:547-561`):
                // muted is determined solely by `mute_until > now`; `silent` controls silent
                // broadcast posts and does NOT mute notifications.
                let muted = parsed.is_muted(now_secs);
                if let Some(peer) = session.peer_mut(peer_id) {
                    peer.common_mut().notify = parsed;
                }
                session.set_history_muted(peer_id, muted);
                seen_peers.push(peer_id);
            }
        }
    });
    if !seen_peers.is_empty()
        && let Ok(mut c) = live.cache.lock()
    {
        for pid in seen_peers {
            c.notify_exception_peers.insert(pid);
        }
    }
}

fn collect_scope_exceptions(
    live: &LiveSession,
    scope: NotifyScope,
    now: i32,
) -> Vec<NotifyException> {
    let extra_peers: Vec<DataPeerId> = live
        .cache
        .lock()
        .map(|c| c.notify_exception_peers.iter().copied().collect())
        .unwrap_or_default();
    live.read_session(|session| {
        let mut candidates = BTreeSet::new();
        for pid in known_peer_ids(session) {
            candidates.insert(pid);
        }
        for pid in extra_peers {
            candidates.insert(pid);
        }
        let mut out = Vec::new();
        for pid in candidates {
            let Some(peer) = session.peer(pid) else {
                continue;
            };
            if !peer_matches_notify_scope(peer, scope) {
                continue;
            }
            let notify = &peer.common().notify;
            let has_explicit = notify.mute_until.is_some() || notify.sound.is_some();
            if !has_explicit {
                continue;
            }
            // Match `NotifySettings::isMuted` (`data/notify/data_notify_settings.cpp:547-561`):
            // `silent` is channel silent-posts, never peer mute.
            let muted = notify.is_muted(now);
            let sound = notify.sound.clone().unwrap_or_default();
            out.push(NotifyException {
                peer: peer_to_settings_ref(peer, now),
                muted,
                sound,
            });
        }
        out.sort_by_key(|e| e.peer.id);
        out
    })
}

/// Fetches scope notification defaults (`account.getNotifySettings`) and per-chat
/// notification exceptions (`account.getNotifyExceptions`), populating `tg_data::Session`.
pub fn fetch_scope_notify_live(live: &LiveSession, scope: NotifyScope) -> ScopeNotifySettings {
    let cached = live
        .cache
        .lock()
        .ok()
        .filter(|c| c.scope_notify_loaded.contains(&scope))
        .and_then(|c| c.scope_notify.get(&scope).cloned());
    if let Some(settings) = cached {
        return settings;
    }

    let mut parsed = match live.invoke_rpc(tl_fns::account::GetNotifySettings {
        peer: notify_scope_to_tl(scope),
    }) {
        Ok(tl_enums::PeerNotifySettings::PeerNotifySettings(resp)) => {
            let parsed = notify_settings_from_tl(&resp);
            let data = peer_notify_settings_from_tl(
                &tl_enums::PeerNotifySettings::PeerNotifySettings(resp),
            );
            let data_scope = notify_scope_to_data_scope(scope);
            let now = now_unix_i32();
            live.write_session(|session| session.set_notify_default(data_scope, Some(data), now));
            parsed
        }
        Err(err) => {
            debug!(?scope, error = %err, "account.getNotifySettings failed");
            ScopeNotifySettings::default()
        }
    };

    let now = now_unix_i32();
    let exceptions_already_loaded = live
        .cache
        .lock()
        .map(|c| c.notify_exceptions_loaded.contains(&scope))
        .unwrap_or(true);
    if !exceptions_already_loaded {
        // Match `NotifySettings::loadExceptions` (`data/notify/data_notify_settings.cpp:663-678`):
        // only sets `Flag::f_peer` (`compare_sound: false, compare_stories: false`).
        match live.invoke_rpc(tl_fns::account::GetNotifyExceptions {
            compare_sound: false,
            compare_stories: false,
            peer: Some(notify_scope_to_tl(scope)),
        }) {
            Ok(updates) => {
                apply_notify_exceptions_updates(live, updates, now);
            }
            Err(err) => {
                debug!(?scope, error = %err, "account.getNotifyExceptions failed");
            }
        }
    }

    parsed.exceptions = collect_scope_exceptions(live, scope, now);
    if let Ok(mut c) = live.cache.lock() {
        c.scope_notify_loaded.insert(scope);
        c.notify_exceptions_loaded.insert(scope);
        c.scope_notify.insert(scope, parsed.clone());
        c.notify_settings_version = c.notify_settings_version.wrapping_add(1);
    }
    parsed
}

/// Saves scope notification defaults and diffs per-peer notification exceptions via
/// `account.updateNotifySettings`, keeping `tg_data::Session` and [`NotifySettingsStore`] in sync.
pub fn save_scope_notify_live(
    live: &mut LiveSession,
    scope: NotifyScope,
    settings: ScopeNotifySettings,
) {
    let prev_exceptions: HashMap<i64, NotifyException> = live
        .cache
        .lock()
        .ok()
        .and_then(|c| c.scope_notify.get(&scope).cloned())
        .map(|prev| {
            prev.exceptions
                .into_iter()
                .map(|e| (e.peer.id, e))
                .collect()
        })
        .unwrap_or_default();

    let scope_req = tl_fns::account::UpdateNotifySettings {
        peer: notify_scope_to_tl(scope),
        settings: notify_settings_to_tl(&settings),
    };
    if let Err(err) = live.invoke_rpc(scope_req) {
        warn!(?scope, error = %err, "account.updateNotifySettings (scope) failed");
        return;
    }
    let data_scope = notify_scope_to_data_scope(scope);
    let data_default = scope_settings_to_data(&settings);
    let now = now_unix_i32();
    live.write_session(|session| session.set_notify_default(data_scope, Some(data_default), now));

    let next_map: BTreeMap<i64, &NotifyException> =
        settings.exceptions.iter().map(|e| (e.peer.id, e)).collect();

    // 1. Apply added or modified per-peer exceptions.
    for (&raw_peer_id, exc) in &next_map {
        let changed = prev_exceptions
            .get(&raw_peer_id)
            .is_none_or(|old| old.muted != exc.muted || old.sound != exc.sound);
        if !changed {
            continue;
        }
        let pid = PeerId(raw_peer_id);
        let mute_until = if exc.muted { i32::MAX } else { 0 };
        let sound_opt = (!exc.sound.is_empty()).then(|| sound_name_to_tl(&exc.sound));
        if let Some(input_peer) = live.input_peer(pid) {
            let req = tl_fns::account::UpdateNotifySettings {
                peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
                    peer: input_peer,
                }),
                settings: tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(
                    tl_types::InputPeerNotifySettings {
                        show_previews: None,
                        // `silent` controls silent broadcast posts, not notification mute
                        // (`NotifyPeerSettingsValue::serialize`, `data_peer_notify_settings.cpp:184-202`).
                        silent: None,
                        mute_until: Some(mute_until),
                        sound: sound_opt,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_sound: None,
                    },
                ),
            };
            if let Err(err) = live.invoke_rpc(req) {
                warn!(peer = raw_peer_id, error = %err, "account.updateNotifySettings (exception) failed");
                continue;
            }
        }
        let data_peer = to_data_peer(pid);
        live.write_session(|session| {
            if let Some(peer) = session.peer_mut(data_peer) {
                peer.common_mut().notify.mute_until = Some(mute_until);
                peer.common_mut().notify.sound = (!exc.sound.is_empty()).then(|| exc.sound.clone());
            }
            session.set_history_muted(data_peer, exc.muted);
        });
        if let Ok(mut c) = live.cache.lock() {
            c.notify_exception_peers.insert(data_peer);
        }
    }

    // 2. Reset removed per-peer exceptions back to scope defaults.
    for &removed_id in prev_exceptions.keys() {
        if next_map.contains_key(&removed_id) {
            continue;
        }
        let pid = PeerId(removed_id);
        if let Some(input_peer) = live.input_peer(pid) {
            let req = tl_fns::account::UpdateNotifySettings {
                peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
                    peer: input_peer,
                }),
                settings: tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(
                    tl_types::InputPeerNotifySettings {
                        show_previews: None,
                        silent: None,
                        mute_until: None,
                        sound: None,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_sound: None,
                    },
                ),
            };
            if let Err(err) = live.invoke_rpc(req) {
                warn!(peer = removed_id, error = %err, "account.updateNotifySettings (reset exception) failed");
                continue;
            }
        }
        let data_peer = to_data_peer(pid);
        live.write_session(|session| {
            if let Some(peer) = session.peer_mut(data_peer) {
                peer.common_mut().notify = DataPeerNotifySettings::default();
            }
            session.set_history_muted(data_peer, !settings.enabled);
        });
    }

    if let Ok(mut c) = live.cache.lock() {
        c.scope_notify_loaded.insert(scope);
        c.notify_exceptions_loaded.insert(scope);
        c.scope_notify.insert(scope, settings);
        c.notify_settings_version = c.notify_settings_version.wrapping_add(1);
    }
    live.bridge.state().notify_ui();
}

/// Synchronizes the per-type defaults, every known per-peer override and every loaded forum
/// topic override from `tg_data::Session` into [`NotifySettingsStore`]
/// (`Data::NotifySettings`, `data/notify/data_notify_settings.cpp`).
///
/// A default that is not loaded yet stays unknown in `store`, so the notification system waits
/// for it (`SkipState::Unknown`) instead of guessing "unmuted".
pub fn sync_notify_settings_store_live(
    live: &LiveSession,
    store: &mut NotifySettingsStore,
    now_unix: i64,
) {
    type TopicSnapshot = (PeerId, MsgId, Option<PeerNotifySettingsValue>);
    // Mention authors whose own settings are still being requested: unknown until they arrive
    // (`NotifySettings::muteUnknown(notifyBy)`).
    let (awaiting, requested): (BTreeSet<DataPeerId>, Vec<DataPeerId>) = live
        .bridge_state()
        .notify_peer_requests
        .lock()
        .map(|requests| (requests.awaiting().collect(), requests.finished().collect()))
        .unwrap_or_default();
    let extra_peers: Vec<DataPeerId> = live
        .cache
        .lock()
        .map(|c| c.notify_exception_peers.iter().copied().collect())
        .unwrap_or_default();

    let (defaults, peer_snapshots, topic_snapshots) = live.read_session(|session| {
        let defaults: Vec<(DefaultNotify, Option<PeerNotifySettingsValue>)> =
            DataDefaultNotifyScope::ALL
                .into_iter()
                .filter_map(|scope| {
                    let value = session.notify_default(scope)?;
                    Some((
                        data_scope_to_default_notify(scope),
                        data_notify_value(value),
                    ))
                })
                .collect();
        let mut candidates = BTreeSet::new();
        candidates.extend(known_peer_ids(session));
        candidates.extend(session.peers_with_notify_settings());
        candidates.extend(extra_peers);
        candidates.extend(awaiting.iter().copied());
        candidates.extend(requested);
        let mut peers = Vec::with_capacity(candidates.len());
        let mut topics: Vec<TopicSnapshot> = Vec::new();
        for data_pid in candidates {
            let Some(peer) = session.peer(data_pid) else {
                continue;
            };
            let peer_id = from_data_peer(data_pid);
            peers.push((
                peer_id,
                data_peer_kind(peer),
                data_notify_value(&peer.common().notify),
            ));
            if let Some(forum) = session.forum(data_pid) {
                for (root_id, topic) in &forum.topics {
                    let value = data_notify_value(&topic.notify);
                    if value.is_some() {
                        topics.push((peer_id, MsgId(root_id.0 as i32), value));
                    }
                }
            }
        }
        (defaults, peers, topics)
    });

    for (scope, value) in defaults {
        store.apply_default(scope, value);
    }
    for (peer_id, kind, snapshot) in peer_snapshots {
        store.register_peer_kind(peer_id, kind);
        if awaiting.contains(&to_data_peer(peer_id)) {
            store.mark_peer_unknown(peer_id);
        } else {
            store.apply_peer(peer_id, snapshot, now_unix);
        }
    }
    for (peer_id, root_id, snapshot) in topic_snapshots {
        store.apply_topic(peer_id, root_id, snapshot, now_unix);
    }
}

/// Maps a `tg_data` default scope to the notification system's [`DefaultNotify`].
const fn data_scope_to_default_notify(scope: DataDefaultNotifyScope) -> DefaultNotify {
    match scope {
        DataDefaultNotifyScope::Users => DefaultNotify::User,
        DataDefaultNotifyScope::Chats => DefaultNotify::Group,
        DataDefaultNotifyScope::Broadcasts => DefaultNotify::Broadcast,
    }
}

/// Maps a settings-screen scope to the `tg_data` default scope.
const fn notify_scope_to_data_scope(scope: NotifyScope) -> DataDefaultNotifyScope {
    match scope {
        NotifyScope::PrivateChats => DataDefaultNotifyScope::Users,
        NotifyScope::Groups => DataDefaultNotifyScope::Chats,
        NotifyScope::Channels => DataDefaultNotifyScope::Broadcasts,
    }
}

/// `tg_data` notification settings as a store snapshot; `None` when nothing is overridden.
fn data_notify_value(notify: &DataPeerNotifySettings) -> Option<PeerNotifySettingsValue> {
    let value = PeerNotifySettingsValue {
        mute_until: notify.mute_until,
        sound: notify.sound.as_deref().map(sound_name_to_notify_sound),
        show_previews: notify.show_previews,
        silent_posts: notify.silent,
        stories_muted: notify.stories_muted,
    };
    (value != PeerNotifySettingsValue::default()).then_some(value)
}

/// The scope default a settings-screen save sends, as `tg_data` settings.
fn scope_settings_to_data(settings: &ScopeNotifySettings) -> DataPeerNotifySettings {
    let sound = match crate::convert::sound_name_to_api_sound(&settings.sound) {
        tg_api::account::NotifySound::Default => "default".to_owned(),
        tg_api::account::NotifySound::None => "none".to_owned(),
        tg_api::account::NotifySound::Ringtone(id) => format!("ringtone:{id}"),
        tg_api::account::NotifySound::Local { data, .. } => data,
    };
    DataPeerNotifySettings {
        mute_until: Some(if settings.enabled { 0 } else { i32::MAX }),
        silent: None,
        show_previews: Some(settings.show_preview),
        sound: Some(sound),
        stories_muted: None,
    }
}

// ============================================================================
// 6. `tdata` persistence & restore helpers (`CoreSettings` + `StoredSessionSettings`)
// ============================================================================

/// Converts tdesktop's persisted [`CoreWindowPosition`] to the shell [`ShellWindowPosition`]
/// if a valid non-empty geometry was recorded.
#[must_use]
pub fn core_window_position_to_shell(pos: CoreWindowPosition) -> Option<ShellWindowPosition> {
    if pos.w <= 0 || pos.h <= 0 {
        return None;
    }
    Some(
        ShellWindowPosition {
            x: Some(pos.x as f32),
            y: Some(pos.y as f32),
            width: pos.w as f32,
            height: pos.h as f32,
            maximized: pos.maximized != 0,
        }
        .clamped(),
    )
}

/// Converts the shell [`ShellWindowPosition`] into tdesktop's 28-byte [`CoreWindowPosition`].
#[must_use]
pub fn shell_window_position_to_core(pos: ShellWindowPosition, scale: i32) -> CoreWindowPosition {
    CoreWindowPosition {
        moncrc: 0,
        maximized: i32::from(pos.maximized),
        scale,
        x: pos.x.unwrap_or(0.0).round() as i32,
        y: pos.y.unwrap_or(0.0).round() as i32,
        w: pos.width.round() as i32,
        h: pos.height.round() as i32,
    }
}

/// Applies restored per-account [`StoredSessionSettings`] and `CoreSettings::autoplay`
/// toggles into [`LiveSession`]'s shared cache on startup.
pub fn apply_stored_session_settings_live(
    live: &mut LiveSession,
    stored: &StoredSessionSettings,
    autoplay: (bool, bool, bool),
) {
    let has_custom_auto_download = stored
        .auto_download
        .raw_limits
        .iter()
        .flatten()
        .any(|&v| v != -1);
    let auto_dl = auto_download_from_stored(&stored.auto_download, autoplay);

    if let Ok(mut c) = live.cache.lock() {
        c.archive_collapsed = stored.archive_collapsed;
        c.phone_hidden = stored.phone_number_hidden;
        c.auto_download = auto_dl;
        c.auto_download_from_tdata = has_custom_auto_download;
        c.trusted_mini_app_bots = stored.trusted_mini_app_bots.clone();
        c.session_settings_dirty = false;
    }
}

/// Captures per-account settings from [`LiveSession`] into a [`StoredSessionSettings`]
/// snapshot (preserving unrecognized/auxiliary tdesktop fields from `base`).
#[must_use]
pub fn collect_stored_session_settings_live(
    live: &LiveSession,
    base: &StoredSessionSettings,
) -> StoredSessionSettings {
    let mut out = base.clone();
    if let Ok(c) = live.cache.lock() {
        out.archive_collapsed = c.archive_collapsed;
        out.phone_number_hidden = c.phone_hidden;
        out.trusted_mini_app_bots = c.trusted_mini_app_bots.clone();
        if c.auto_download_from_tdata || c.auto_download_loaded {
            let mut converted = auto_download_to_stored(&c.auto_download);
            converted.peer_overrides = base.auto_download.peer_overrides.clone();
            out.auto_download = converted;
        }
    }
    out
}

/// Persists `GlobalConfig`, `CoreSettings`, and (optionally) per-account [`StoredSessionSettings`]
/// into `tdata` through [`AccountManager`].
pub fn persist_all_settings_to_tdata(
    manager: &mut AccountManager,
    account_index: i32,
    core: &CoreSettings,
    global: &GlobalConfig,
    session_settings: Option<&StoredSessionSettings>,
) -> tg_session::Result<()> {
    manager.save_global_and_core_settings(core, global)?;
    if let Some(ss) = session_settings {
        manager.save_session_settings(account_index, ss)?;
    }
    Ok(())
}
