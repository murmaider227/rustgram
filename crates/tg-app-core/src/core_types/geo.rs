//! Pure platform geolocation provider helper and Mini App location JSON builders.
//!
//! Port origin:
//! - `Telegram/SourceFiles/platform/win/current_geo_location_win.cpp`
//! - `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` (location events)

use std::sync::Arc;

use tg_platform::geolocation::{
    GeoLocation, GeoLocationProvider, GeolocationStatus, WinRtGeoLocationProvider,
};

/// Returns the production Windows WinRT geolocation provider behind [`GeoLocationProvider`].
#[must_use]
pub fn default_geolocation_provider() -> Arc<dyn GeoLocationProvider> {
    Arc::new(WinRtGeoLocationProvider::new())
}

/// Builds the Telegram Mini App `location_checked` event JSON payload for `web_app_check_location`.
///
/// `access` is the per-bot consent given in the panel (`None` = the user was never asked);
/// access counts as granted only while the OS location service is also usable.
#[must_use]
pub fn mini_app_location_checked_json(
    status: GeolocationStatus,
    access: Option<bool>,
) -> serde_json::Value {
    let available = !matches!(status, GeolocationStatus::NotAvailable);
    let os_usable = !matches!(
        status,
        GeolocationStatus::Disabled | GeolocationStatus::NotAvailable
    );
    let access_requested = access.is_some();
    let access_granted = access == Some(true) && os_usable;
    serde_json::json!({
        "available": available,
        "access_requested": access_requested,
        "access_granted": access_granted,
    })
}

/// Builds the Telegram Mini App `location_requested` event JSON payload for
/// `web_app_request_location`.
#[must_use]
pub fn mini_app_location_requested_json(location: Option<GeoLocation>) -> serde_json::Value {
    match location {
        Some(loc) if loc.point.is_valid() => {
            let mut obj = serde_json::Map::new();
            obj.insert("available".to_owned(), serde_json::Value::Bool(true));
            obj.insert(
                "latitude".to_owned(),
                serde_json::Value::from(loc.point.latitude),
            );
            obj.insert(
                "longitude".to_owned(),
                serde_json::Value::from(loc.point.longitude),
            );
            if let Some(acc) = loc.horizontal_accuracy_meters {
                obj.insert(
                    "horizontal_accuracy".to_owned(),
                    serde_json::Value::from(acc),
                );
            }
            serde_json::Value::Object(obj)
        }
        _ => serde_json::json!({ "available": false }),
    }
}
