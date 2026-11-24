//! Live Telegram static map tiles (`upload.getWebFile(InputWebFileGeoPointLocation)`) and
//! Windows geolocation wiring helpers for `LocationPickerBoxState`, bot `RequestLocation`, and
//! Mini App `web_app_check_location` / `web_app_request_location`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_location.cpp` (`ComputeLocation`)
//! - `Telegram/SourceFiles/platform/win/current_geo_location_win.cpp`
//! - `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` (location events)

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tg_api::RpcClient;
use tg_app_core::core_types::country_bounds::{country_bounds, country_iso2_from_phone};
use tg_app_core::core_types::platform_call::PlatformCall;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::session::LiveSession;

/// Chunk size requested per `upload.getWebFile` call (`128 KiB`, matches tdesktop webfile loader).
const STATIC_MAP_CHUNK_LIMIT: i32 = 128 * 1024;
/// Upper safety cap on downloaded static map tile bytes (`2 MiB`).
const MAX_STATIC_MAP_BYTES: usize = 2 * 1024 * 1024;

/// Parameters for `upload.getWebFile(InputWebFileGeoPointLocation)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StaticMapTileRequest {
    /// Pin latitude in WGS84 degrees (`-90..=90`).
    pub lat: f64,
    /// Pin longitude in WGS84 degrees (`-180..=180`).
    pub lon: f64,
    /// Optional server `GeoPoint.access_hash` (`0` for user-picked coordinates).
    pub access_hash: i64,
    /// Tile width in logical pixels (`16..=1024`).
    pub width: i32,
    /// Tile height in logical pixels (`16..=1024`).
    pub height: i32,
    /// Telegram map zoom level (`13..=20`, matching `Data::ComputeLocation`).
    pub zoom: i32,
    /// DPI scale factor (`1..=3`).
    pub scale: i32,
}

impl StaticMapTileRequest {
    /// Builds a sanitized static map tile request from picker UI zoom (`1..=5`) and pixel bounds.
    #[must_use]
    pub fn from_picker(
        lat: f64,
        lon: f64,
        width: i32,
        height: i32,
        picker_zoom: u8,
        scale: u8,
    ) -> Self {
        let clamped_scale = i32::from(scale.clamp(1, 3));
        // Picker zoom `1..=5` maps to Telegram static map zoom `12..=16` (zoom 3 -> 14, matching
        // `Data::ComputeLocation` baseline 13 + (scale - 1)).
        let zoom = (11 + i32::from(picker_zoom.clamp(1, 5)) + (clamped_scale - 1)).clamp(12, 20);
        Self {
            lat: lat.clamp(-85.0511, 85.0511),
            lon: lon.clamp(-180.0, 180.0),
            access_hash: 0,
            width: width.clamp(16, 1024),
            height: height.clamp(16, 1024),
            zoom,
            scale: clamped_scale,
        }
    }

    /// Converts this request into a TL `InputWebFileLocation::InputWebFileGeoPointLocation`.
    #[must_use]
    pub fn to_tl_location(self) -> tl_enums::InputWebFileLocation {
        tl_enums::InputWebFileLocation::InputWebFileGeoPointLocation(
            tl_types::InputWebFileGeoPointLocation {
                geo_point: tl_enums::InputGeoPoint::InputGeoPoint(tl_types::InputGeoPoint {
                    lat: self.lat,
                    long: self.lon,
                    accuracy_radius: None,
                }),
                access_hash: self.access_hash,
                w: self.width,
                h: self.height,
                zoom: self.zoom,
                scale: self.scale,
            },
        )
    }
}

/// Downloads a Telegram static map tile image via `upload.getWebFile(InputWebFileGeoPointLocation)`.
pub async fn fetch_static_map_tile_async<C: RpcClient>(
    rpc: &C,
    req: StaticMapTileRequest,
) -> Result<Vec<u8>, String> {
    if !req.lat.is_finite() || !req.lon.is_finite() {
        return Err("Map coordinates must be finite WGS84 degrees.".to_owned());
    }

    let mut out = Vec::new();
    let mut offset: i32 = 0;

    loop {
        let tl_req = tl_fns::upload::GetWebFile {
            location: req.to_tl_location(),
            offset,
            limit: STATIC_MAP_CHUNK_LIMIT,
        };
        let resp = rpc.invoke(&tl_req).await.map_err(|e| {
            let inv: tg_api::InvocationError = e.into();
            format!("upload.getWebFile failed: {inv}")
        })?;
        let tl_enums::upload::WebFile::WebFile(wf) = resp;
        let chunk_len = wf.bytes.len();
        if chunk_len == 0 {
            break;
        }
        if out.len().saturating_add(chunk_len) > MAX_STATIC_MAP_BYTES {
            return Err("Static map tile exceeded maximum allowed size.".to_owned());
        }
        out.extend_from_slice(&wf.bytes);
        offset = offset.saturating_add(chunk_len as i32);

        let expected_size = usize::try_from(wf.size).unwrap_or(0);
        if (expected_size > 0 && out.len() >= expected_size)
            || chunk_len < STATIC_MAP_CHUNK_LIMIT as usize
        {
            break;
        }
    }

    if out.is_empty() {
        return Err("Telegram returned an empty static map tile.".to_owned());
    }
    Ok(out)
}

/// Synchronous hook for [`tg_app_core::history::backend::HistoryBackend::load_static_map_tile`].
pub(super) fn load_static_map_tile(
    live: &LiveSession,
    lat: f64,
    lon: f64,
    width: i32,
    height: i32,
    zoom: u8,
    scale: u8,
) -> Result<Vec<u8>, String> {
    let req = StaticMapTileRequest::from_picker(lat, lon, width, height, zoom, scale);
    let rpc = live.runtime().rpc().clone();
    crate::media::block_on_runtime(live.runtime().tokio_handle(), async move {
        fetch_static_map_tile_async(&rpc, req).await
    })
}

/// Non-blocking async hook for
/// [`tg_app_core::history::backend::HistoryBackend::load_static_map_tile_async`].
///
/// Spawns `upload.getWebFile(InputWebFileGeoPointLocation)` on the session's `tokio` runtime
/// instead of blocking the UI frame loop, and requests an egui repaint when bytes arrive.
#[allow(clippy::too_many_arguments)]
pub(super) fn load_static_map_tile_async(
    live: &LiveSession,
    lat: f64,
    lon: f64,
    width: i32,
    height: i32,
    zoom: u8,
    scale: u8,
    repaint: Option<egui::Context>,
) -> PlatformCall<Result<Vec<u8>, String>> {
    let req = StaticMapTileRequest::from_picker(lat, lon, width, height, zoom, scale);
    let rpc = live.runtime().rpc().clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    live.runtime().tokio_handle().spawn(async move {
        let outcome = fetch_static_map_tile_async(&rpc, req).await;
        let _ = tx.send(outcome);
        if let Some(ctx) = repaint {
            ctx.request_repaint();
        }
    });
    PlatformCall::from_receiver(rx)
}

struct CachedNearestDcCountry {
    owner: Weak<LiveBridgeState>,
    iso2: Option<String>,
}

fn nearest_dc_country_cache() -> &'static Mutex<HashMap<usize, CachedNearestDcCountry>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, CachedNearestDcCountry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolves the user's ISO 3166-1 alpha-2 country code for the location picker's default map
/// centre without triggering an OS geolocation prompt.
///
/// Precedence mirrors tdesktop:
/// 1. Logged-in account phone number country (`tg-lang::countries::country_iso2_by_phone`).
/// 2. Cached `help.getNearestDc` country (fetched once per session when no phone country exists).
pub(super) fn default_location_country_iso2(live: &LiveSession) -> Option<String> {
    let phone = live.read_session(|s| {
        s.user(s.self_user_id())
            .map(|u| u.phone.clone())
            .filter(|p| !p.trim().is_empty())
    });
    if let Some(phone) = phone
        && let Some(iso2) = country_iso2_from_phone(&phone)
    {
        return Some(iso2);
    }

    let key = Arc::as_ptr(live.bridge_state()) as usize;
    {
        let mut cache = nearest_dc_country_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|_, entry| entry.owner.strong_count() > 0);
        if let Some(entry) = cache.get(&key) {
            return entry.iso2.clone();
        }
    }

    let fetched = match live.invoke_rpc(tl_fns::help::GetNearestDc {}) {
        Ok(tl_enums::NearestDc::NearestDc(dc)) => {
            let iso2 = dc.country.trim().to_ascii_uppercase();
            if country_bounds(&iso2).is_some() {
                Some(iso2)
            } else {
                None
            }
        }
        Err(_) => None,
    };

    if fetched.is_some() {
        let mut cache = nearest_dc_country_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.insert(
            key,
            CachedNearestDcCountry {
                owner: Arc::downgrade(live.bridge_state()),
                iso2: fetched.clone(),
            },
        );
    }
    fetched
}

/// Decodes raw PNG/JPEG static map tile bytes into `(width, height, rgba8)` pixels.
pub fn decode_map_tile_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| format!("Failed to decode static map tile image: {e}"))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Err("Decoded static map tile has zero dimensions.".to_owned());
    }
    Ok((w, h, rgba.into_raw()))
}

pub use tg_app_core::core_types::geo::{
    default_geolocation_provider, mini_app_location_checked_json, mini_app_location_requested_json,
};
