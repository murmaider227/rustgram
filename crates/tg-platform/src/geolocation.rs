//! Native Windows geolocation via WinRT `Windows.Devices.Geolocation.Geolocator`,
//! behind a testable [`GeoLocationProvider`] trait with a deterministic [`FakeGeoLocationProvider`].
//!
//! Port of:
//! - `Telegram/SourceFiles/core/current_geo_location.h`
//! - `Telegram/SourceFiles/platform/platform_current_geo_location.h`
//! - `Telegram/SourceFiles/platform/win/current_geo_location_win.{h,cpp}`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::Result;

/// Geographic coordinates in WGS84 degrees (`Core::GeoPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GeoPoint {
    /// Latitude in degrees (`-90.0..=90.0`).
    pub latitude: f64,
    /// Longitude in degrees (`-180.0..=180.0`).
    pub longitude: f64,
}

impl GeoPoint {
    /// Creates a new [`GeoPoint`].
    #[must_use]
    pub const fn new(latitude: f64, longitude: f64) -> Self {
        Self {
            latitude,
            longitude,
        }
    }

    /// Returns `true` when latitude and longitude are finite and within WGS84 bounds.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
    }
}

/// Precision class of a resolved [`GeoLocation`] (`Core::GeoLocationAccuracy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GeoLocationAccuracy {
    /// Exact GPS/Wi-Fi/WinRT location reading.
    #[default]
    Exact,
    /// Coarse/approximate IP or cell-tower reading.
    Approximate,
}

/// Current geographic location fix (`Core::GeoLocation`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoLocation {
    /// Coordinates in degrees.
    pub point: GeoPoint,
    /// Accuracy classification.
    pub accuracy: GeoLocationAccuracy,
    /// Optional horizontal accuracy radius reported by WinRT in meters.
    pub horizontal_accuracy_meters: Option<f64>,
}

impl GeoLocation {
    /// Creates an exact location fix.
    #[must_use]
    pub const fn exact(latitude: f64, longitude: f64) -> Self {
        Self {
            point: GeoPoint::new(latitude, longitude),
            accuracy: GeoLocationAccuracy::Exact,
            horizontal_accuracy_meters: None,
        }
    }
}

/// Reverse-geocoded human-readable address (`Core::GeoAddress`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeoAddress {
    /// Country name or ISO code.
    pub country: String,
    /// State / province / region.
    pub state: String,
    /// City / locality.
    pub city: String,
    /// Street name.
    pub street: String,
}

/// Availability status reported by WinRT `Windows.Devices.Geolocation.PositionStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GeolocationStatus {
    /// Location data is available.
    Ready,
    /// Location provider is initializing.
    Initializing,
    /// No location data is currently available.
    NoData,
    /// Location access is disabled by OS settings or user permission.
    Disabled,
    /// Location operation has not been initialized yet.
    NotInitialized,
    /// Platform has no location provider available.
    #[default]
    NotAvailable,
}

/// Abstraction over OS geolocation services so UI and tests can swap in [`FakeGeoLocationProvider`].
pub trait GeoLocationProvider: Send + Sync {
    /// Queries the current platform geolocation status without triggering a fix.
    fn status(&self) -> GeolocationStatus;

    /// Resolves the device's current exact geographic coordinates.
    ///
    /// Returns `Ok(None)` when location services are unavailable or denied, mirroring
    /// `Platform::ResolveCurrentExactLocation` calling back with an empty `Core::GeoLocation`.
    fn resolve_current_exact_location(&self) -> Result<Option<GeoLocation>>;

    /// Reverse-geocodes `location` in `language`.
    ///
    /// Matches `current_geo_location_win.cpp:62-67`, where Windows returns `Ok(None)`.
    fn resolve_location_address(
        &self,
        _location: &GeoLocation,
        _language: &str,
    ) -> Result<Option<GeoAddress>> {
        Ok(None)
    }
}

/// Native Windows WinRT `Windows.Devices.Geolocation.Geolocator` provider.
///
/// Runs WinRT calls on a dedicated MTA worker thread so calling from an STA GUI thread never
/// deadlocks while waiting on `IAsyncOperation<Geoposition>`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinRtGeoLocationProvider;

impl WinRtGeoLocationProvider {
    /// Creates a new [`WinRtGeoLocationProvider`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl GeoLocationProvider for WinRtGeoLocationProvider {
    fn status(&self) -> GeolocationStatus {
        #[cfg(windows)]
        {
            winrt_impl::query_status()
        }
        #[cfg(not(windows))]
        {
            GeolocationStatus::NotAvailable
        }
    }

    fn resolve_current_exact_location(&self) -> Result<Option<GeoLocation>> {
        #[cfg(windows)]
        {
            winrt_impl::resolve_exact_location()
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }
}

/// Deterministic in-memory [`GeoLocationProvider`] for tests and headless environments.
#[derive(Debug, Clone)]
pub struct FakeGeoLocationProvider {
    inner: Arc<Mutex<FakeGeoState>>,
    resolve_calls: Arc<AtomicUsize>,
    address_calls: Arc<AtomicUsize>,
}

#[derive(Debug, Clone)]
struct FakeGeoState {
    status: GeolocationStatus,
    location: Option<GeoLocation>,
    address: Option<GeoAddress>,
    error_message: Option<String>,
}

impl Default for FakeGeoLocationProvider {
    fn default() -> Self {
        Self::with_location(GeoLocation::exact(37.7749, -122.4194))
    }
}

impl FakeGeoLocationProvider {
    /// Creates a fake provider that succeeds with `location`.
    #[must_use]
    pub fn with_location(location: GeoLocation) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeGeoState {
                status: GeolocationStatus::Ready,
                location: Some(location),
                address: None,
                error_message: None,
            })),
            resolve_calls: Arc::new(AtomicUsize::new(0)),
            address_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Creates a fake provider that simulates unavailable/disabled OS location services.
    #[must_use]
    pub fn unavailable(status: GeolocationStatus) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeGeoState {
                status,
                location: None,
                address: None,
                error_message: None,
            })),
            resolve_calls: Arc::new(AtomicUsize::new(0)),
            address_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Updates the fake location returned by subsequent lookups.
    pub fn set_location(&self, location: Option<GeoLocation>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.location = location;
        }
    }

    /// Updates the fake reverse-geocoded address.
    pub fn set_address(&self, address: Option<GeoAddress>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.address = address;
        }
    }

    /// Injects an error to be returned by subsequent lookups.
    pub fn set_error(&self, message: Option<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.error_message = message;
        }
    }

    /// Number of times [`GeoLocationProvider::resolve_current_exact_location`] was invoked.
    #[must_use]
    pub fn resolve_call_count(&self) -> usize {
        self.resolve_calls.load(Ordering::SeqCst)
    }

    /// Number of times [`GeoLocationProvider::resolve_location_address`] was invoked.
    #[must_use]
    pub fn address_call_count(&self) -> usize {
        self.address_calls.load(Ordering::SeqCst)
    }
}

impl GeoLocationProvider for FakeGeoLocationProvider {
    fn status(&self) -> GeolocationStatus {
        self.inner
            .lock()
            .map(|g| g.status)
            .unwrap_or(GeolocationStatus::NotAvailable)
    }

    fn resolve_current_exact_location(&self) -> Result<Option<GeoLocation>> {
        self.resolve_calls.fetch_add(1, Ordering::SeqCst);
        let guard = self
            .inner
            .lock()
            .map_err(|_| crate::error::PlatformError::Geolocation("lock poisoned".to_owned()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(crate::error::PlatformError::Geolocation(msg.clone()));
        }
        if matches!(
            guard.status,
            GeolocationStatus::NotAvailable | GeolocationStatus::Disabled
        ) {
            return Ok(None);
        }
        Ok(guard.location)
    }

    fn resolve_location_address(
        &self,
        _location: &GeoLocation,
        _language: &str,
    ) -> Result<Option<GeoAddress>> {
        self.address_calls.fetch_add(1, Ordering::SeqCst);
        let guard = self
            .inner
            .lock()
            .map_err(|_| crate::error::PlatformError::Geolocation("lock poisoned".to_owned()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(crate::error::PlatformError::Geolocation(msg.clone()));
        }
        Ok(guard.address.clone())
    }
}

#[cfg(windows)]
mod winrt_impl {
    use windows::Devices::Geolocation::{Geolocator, PositionAccuracy, PositionStatus};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};

    use super::{GeoLocation, GeoLocationAccuracy, GeoPoint, GeolocationStatus};
    use crate::error::{PlatformError, Result};

    struct RoInitGuard(bool);

    impl RoInitGuard {
        fn enter_mta() -> Self {
            // SAFETY: WinRT apartment initialization on a dedicated worker thread; balanced by
            // `RoUninitialize` in `Drop` only when `RoInitialize` succeeded.
            let ok = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
            Self(ok)
        }
    }

    impl Drop for RoInitGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Matches the successful `RoInitialize` call on the same thread.
                unsafe { RoUninitialize() };
            }
        }
    }

    fn map_position_status(status: PositionStatus) -> GeolocationStatus {
        match status {
            PositionStatus::Ready => GeolocationStatus::Ready,
            PositionStatus::Initializing => GeolocationStatus::Initializing,
            PositionStatus::NoData => GeolocationStatus::NoData,
            PositionStatus::Disabled => GeolocationStatus::Disabled,
            PositionStatus::NotInitialized => GeolocationStatus::NotInitialized,
            _ => GeolocationStatus::NotAvailable,
        }
    }

    pub(super) fn query_status() -> GeolocationStatus {
        std::thread::spawn(|| {
            let _guard = RoInitGuard::enter_mta();
            let Ok(geolocator) = Geolocator::new() else {
                return GeolocationStatus::NotAvailable;
            };
            geolocator
                .LocationStatus()
                .map(map_position_status)
                .unwrap_or(GeolocationStatus::NotAvailable)
        })
        .join()
        .unwrap_or(GeolocationStatus::NotAvailable)
    }

    pub(super) fn resolve_exact_location() -> Result<Option<GeoLocation>> {
        let handle = std::thread::Builder::new()
            .name("tg-winrt-geolocation".to_owned())
            .spawn(|| -> Result<Option<GeoLocation>> {
                let _guard = RoInitGuard::enter_mta();
                let geolocator =
                    Geolocator::new().map_err(|e| PlatformError::from_win("Geolocator::new", e))?;
                let _ = geolocator.SetDesiredAccuracy(PositionAccuracy::High);
                let status = geolocator
                    .LocationStatus()
                    .map_err(|e| PlatformError::from_win("Geolocator::LocationStatus", e))?;
                if status == PositionStatus::NotAvailable || status == PositionStatus::Disabled {
                    return Ok(None);
                }
                let async_op = geolocator
                    .GetGeopositionAsync()
                    .map_err(|e| PlatformError::from_win("Geolocator::GetGeopositionAsync", e))?;
                let geoposition = async_op
                    .join()
                    .map_err(|e| PlatformError::from_win("GetGeopositionAsync::join", e))?;
                let coord = geoposition
                    .Coordinate()
                    .map_err(|e| PlatformError::from_win("Geoposition::Coordinate", e))?;
                let pos = coord
                    .Point()
                    .and_then(|p| p.Position())
                    .map_err(|e| PlatformError::from_win("Geocoordinate::Point", e))?;
                let accuracy_m = coord.Accuracy().ok().filter(|v| v.is_finite() && *v >= 0.0);
                let point = GeoPoint::new(pos.Latitude, pos.Longitude);
                if !point.is_valid() {
                    return Ok(None);
                }
                Ok(Some(GeoLocation {
                    point,
                    accuracy: GeoLocationAccuracy::Exact,
                    horizontal_accuracy_meters: accuracy_m,
                }))
            })
            .map_err(PlatformError::Io)?;

        handle.join().map_err(|_| {
            PlatformError::Geolocation("WinRT geolocation thread panicked".to_owned())
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geo_point_bounds_validation() {
        assert!(GeoPoint::new(51.5074, -0.1278).is_valid());
        assert!(GeoPoint::new(-90.0, 180.0).is_valid());
        assert!(!GeoPoint::new(91.0, 0.0).is_valid());
        assert!(!GeoPoint::new(0.0, -181.0).is_valid());
        assert!(!GeoPoint::new(f64::NAN, 0.0).is_valid());
    }

    #[test]
    fn fake_provider_returns_configured_location_and_address() {
        let provider = FakeGeoLocationProvider::with_location(GeoLocation {
            point: GeoPoint::new(48.8566, 2.3522),
            accuracy: GeoLocationAccuracy::Exact,
            horizontal_accuracy_meters: Some(15.0),
        });
        provider.set_address(Some(GeoAddress {
            country: "FR".to_owned(),
            state: "Ile-de-France".to_owned(),
            city: "Paris".to_owned(),
            street: "Rue de Rivoli".to_owned(),
        }));

        assert_eq!(provider.status(), GeolocationStatus::Ready);
        let loc = provider
            .resolve_current_exact_location()
            .expect("lookup should succeed")
            .expect("location should be present");
        assert_eq!(loc.point, GeoPoint::new(48.8566, 2.3522));
        assert_eq!(loc.horizontal_accuracy_meters, Some(15.0));

        let addr = provider
            .resolve_location_address(&loc, "en")
            .expect("address lookup should succeed")
            .expect("address should be present");
        assert_eq!(addr.city, "Paris");
        assert_eq!(provider.resolve_call_count(), 1);
        assert_eq!(provider.address_call_count(), 1);
    }

    #[test]
    fn fake_provider_handles_disabled_and_errors() {
        let disabled = FakeGeoLocationProvider::unavailable(GeolocationStatus::Disabled);
        assert_eq!(disabled.status(), GeolocationStatus::Disabled);
        assert_eq!(disabled.resolve_current_exact_location().unwrap(), None);

        disabled.set_error(Some("location denied".to_owned()));
        let err = disabled.resolve_current_exact_location().unwrap_err();
        assert!(err.to_string().contains("location denied"));
    }
}
