//! Windows caBLEv2 BLE proximity scanner and FIDO2/Google service-data EID parser.
//!
//! Port of:
//! - `Telegram/SourceFiles/webauthn/cable_scanner.h`
//! - `Telegram/SourceFiles/webauthn/cable_scanner_win.cpp`
//!
//! Provides:
//! - Pure-Rust BLE advertisement & 16-bit Service Data (`0x16`) parser for FIDO2 (`0xFFF9`)
//!   and Google (`0xFDE2`) 20-byte caBLEv2 encrypted EID payloads;
//! - [`CableBleScanner`] trait mirroring `Platform::WebAuthn::Cable::BleScanner`;
//! - [`WinRtCableBleScanner`] backed by WinRT `BluetoothLEAdvertisementWatcher` under
//!   `#[cfg(windows)]` (with dynamic `bthprops.cpl` radio detection);
//! - [`FakeCableBleScanner`] for deterministic tests without touching real Bluetooth hardware.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::{PlatformError, Result};
use crate::webauthn::{
    CABLE_ADVERT_LEN, CABLE_EID_KEY_LEN, CABLE_EID_LEN, CableEidComponents,
    cable_decode_tunnel_server_domain, cable_decrypt_advert, cable_to_eid_components,
};

/// Bluetooth GAP Advertising Data type for "Service Data - 16-bit UUID" (`kServiceDataAdType`).
pub const BLE_AD_TYPE_SERVICE_DATA_16: u8 = 0x16;

/// Google caBLEv2 16-bit BLE service UUID (`kGoogleCableUuid16`, `cable_scanner_win.cpp:36`).
pub const CABLE_GOOGLE_UUID16: u16 = 0xFDE2;

/// FIDO Alliance caBLEv2 / hybrid 16-bit BLE service UUID (`kFidoCableUuid16`, `cable_scanner_win.cpp:37`).
pub const CABLE_FIDO_UUID16: u16 = 0xFFF9;

/// Minimum byte length of a valid 16-bit Service Data section containing a caBLEv2 advert:
/// 2-byte little-endian UUID16 + 20-byte encrypted EID advert.
pub const CABLE_SERVICE_DATA_MIN_LEN: usize = 2 + CABLE_ADVERT_LEN;

/// Callback invoked for each newly observed caBLE BLE service-data payload (20+ bytes).
pub type AdvertCallback = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Callback invoked once Bluetooth availability is known (`true` = scanning active).
pub type AvailabilityCallback = Arc<dyn Fn(bool) + Send + Sync>;

/// Recognized 16-bit BLE service UUID carrying a caBLEv2 EID advertisement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CableServiceUuid {
    /// FIDO Alliance hybrid/caBLE service UUID (`0xFFF9`).
    Fido,
    /// Legacy Google caBLE service UUID (`0xFDE2`).
    Google,
}

impl CableServiceUuid {
    /// Parses a 16-bit little-endian UUID into a recognized caBLE service UUID.
    #[must_use]
    pub const fn from_u16(uuid: u16) -> Option<Self> {
        match uuid {
            CABLE_FIDO_UUID16 => Some(Self::Fido),
            CABLE_GOOGLE_UUID16 => Some(Self::Google),
            _ => None,
        }
    }

    /// Returns the numeric 16-bit UUID value.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::Fido => CABLE_FIDO_UUID16,
            Self::Google => CABLE_GOOGLE_UUID16,
        }
    }
}

/// One Advertising Data (AD) section inside a BLE advertisement packet.
///
/// Mirrors `Windows.Devices.Bluetooth.Advertisement.BluetoothLEAdvertisementDataSection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleDataSection {
    /// GAP Advertising Data type byte (e.g. [`BLE_AD_TYPE_SERVICE_DATA_16`] = `0x16`).
    pub data_type: u8,
    /// Raw section payload bytes (excluding length and type bytes).
    pub data: Vec<u8>,
}

/// Parsed BLE advertisement consisting of one or more AD sections plus optional radio metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BleAdvertisement {
    /// GAP Advertising Data sections present in the packet.
    pub sections: Vec<BleDataSection>,
    /// Optional 48-bit Bluetooth device address reported by the OS.
    pub bluetooth_address: Option<u64>,
    /// Optional received signal strength indicator in dBm.
    pub rssi: Option<i16>,
}

/// Extracted caBLEv2 BLE service-data payload from a single `0x16` AD section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CableServiceData {
    /// Matched 16-bit service UUID (`0xFFF9` or `0xFDE2`).
    pub uuid: CableServiceUuid,
    /// Service data bytes following the 2-byte UUID (at least [`CABLE_ADVERT_LEN`] = 20 bytes).
    pub service_data: Vec<u8>,
}

impl CableServiceData {
    /// Returns the 20-byte caBLEv2 advert prefix (`16`-byte ciphertext + `4`-byte HMAC tag).
    #[must_use]
    pub fn advert_bytes(&self) -> Option<&[u8; CABLE_ADVERT_LEN]> {
        self.service_data.get(..CABLE_ADVERT_LEN)?.try_into().ok()
    }
}

/// Decrypted and authenticated caBLEv2 proximity match ready for tunnel connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedCableEid {
    /// Service UUID on which the advert was received (if parsed from a full section).
    pub uuid: Option<CableServiceUuid>,
    /// Raw service-data payload bytes (20+ bytes).
    pub raw_service_data: Vec<u8>,
    /// Decrypted 16-byte plaintext EID.
    pub decrypted_eid: [u8; CABLE_EID_LEN],
    /// Parsed nonce, routing ID, and tunnel server domain identifier.
    pub components: CableEidComponents,
    /// Resolved caBLEv2 WebSocket tunnel server domain (e.g. `"cable.ua5v.com"`).
    pub tunnel_domain: String,
}

/// Parses a raw BLE GAP Advertising Data buffer (sequence of `[length, ad_type, payload...]`)
/// into [`BleDataSection`] entries.
///
/// Stops cleanly on a zero-length terminator or truncated trailing structure without panicking.
#[must_use]
pub fn parse_raw_ble_ad_packet(raw: &[u8]) -> Vec<BleDataSection> {
    let mut sections = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        let len = usize::from(raw[offset]);
        offset += 1;
        if len == 0 {
            break;
        }
        if offset + len > raw.len() {
            break;
        }
        let data_type = raw[offset];
        let payload = raw[offset + 1..offset + len].to_vec();
        sections.push(BleDataSection {
            data_type,
            data: payload,
        });
        offset += len;
    }
    sections
}

/// Parses one BLE AD section for a caBLEv2 service-data payload (`CableServiceData` in
/// `cable_scanner_win.cpp:39-59`).
///
/// Returns `Some` only when `data_type == 0x16`, `data.len() >= 22`, and the leading 16-bit
/// little-endian UUID is either `0xFFF9` (FIDO2) or `0xFDE2` (Google).
#[must_use]
pub fn parse_cable_service_data_section(data_type: u8, data: &[u8]) -> Option<CableServiceData> {
    if data_type != BLE_AD_TYPE_SERVICE_DATA_16 {
        return None;
    }
    if data.len() < CABLE_SERVICE_DATA_MIN_LEN {
        return None;
    }
    let uuid_raw = u16::from_le_bytes([data[0], data[1]]);
    let uuid = CableServiceUuid::from_u16(uuid_raw)?;
    Some(CableServiceData {
        uuid,
        service_data: data[2..].to_vec(),
    })
}

/// Extracts all valid caBLEv2 service-data payloads from a [`BleAdvertisement`].
#[must_use]
pub fn extract_cable_service_data(advertisement: &BleAdvertisement) -> Vec<CableServiceData> {
    advertisement
        .sections
        .iter()
        .filter_map(|section| parse_cable_service_data_section(section.data_type, &section.data))
        .collect()
}

/// Builds a BLE `0x16` Service Data section containing `uuid` (little-endian) followed by `advert`.
#[must_use]
pub fn build_cable_service_data_section(uuid: CableServiceUuid, advert: &[u8]) -> BleDataSection {
    let mut data = Vec::with_capacity(2 + advert.len());
    data.extend_from_slice(&uuid.as_u16().to_le_bytes());
    data.extend_from_slice(advert);
    BleDataSection {
        data_type: BLE_AD_TYPE_SERVICE_DATA_16,
        data,
    }
}

/// Attempts to authenticate and decrypt raw caBLEv2 service-data bytes against `eid_key`.
#[must_use]
pub fn match_cable_service_data_bytes(
    service_data: &[u8],
    eid_key: &[u8; CABLE_EID_KEY_LEN],
    uuid: Option<CableServiceUuid>,
) -> Option<MatchedCableEid> {
    let decrypted_eid = cable_decrypt_advert(service_data, eid_key)?;
    let components = cable_to_eid_components(&decrypted_eid);
    let tunnel_domain = cable_decode_tunnel_server_domain(components.tunnel_server_domain)?;
    Some(MatchedCableEid {
        uuid,
        raw_service_data: service_data.to_vec(),
        decrypted_eid,
        components,
        tunnel_domain,
    })
}

/// Attempts to authenticate and decrypt a parsed [`CableServiceData`] payload against `eid_key`.
#[must_use]
pub fn match_cable_service_data(
    section: &CableServiceData,
    eid_key: &[u8; CABLE_EID_KEY_LEN],
) -> Option<MatchedCableEid> {
    match_cable_service_data_bytes(&section.service_data, eid_key, Some(section.uuid))
}

/// Scans all sections of `advertisement` and returns the first caBLEv2 EID matching `eid_key`.
#[must_use]
pub fn match_cable_advertisement(
    advertisement: &BleAdvertisement,
    eid_key: &[u8; CABLE_EID_KEY_LEN],
) -> Option<MatchedCableEid> {
    for section in extract_cable_service_data(advertisement) {
        if let Some(matched) = match_cable_service_data(&section, eid_key) {
            return Some(matched);
        }
    }
    None
}

/// Abstraction over platform BLE advertisement scanning for caBLEv2 passkey proximity
/// (`Platform::WebAuthn::Cable::BleScanner` in `cable_scanner.h`).
pub trait CableBleScanner: Send + Sync {
    /// Starts scanning for caBLEv2 (`0xFFF9` / `0xFDE2`) BLE advertisements.
    ///
    /// Returns `Ok(true)` when scanning started, `Ok(false)` when no Bluetooth radio is present,
    /// or `Err` on platform failure.
    fn start(
        &self,
        on_advert: AdvertCallback,
        on_availability: AvailabilityCallback,
    ) -> Result<bool>;

    /// Stops scanning and releases any OS watcher / callback registrations.
    fn stop(&self);

    /// Returns `true` while the scanner is actively watching for advertisements.
    fn is_running(&self) -> bool;
}

/// Helper that wires a [`CableBleScanner`] to a specific caBLEv2 `eid_key` and invokes `on_match`
/// when a valid, authenticated EID advertisement is discovered (matching `OnAdvert` in
/// `cable_ceremony.cpp:280-299`).
pub fn start_cable_proximity_match<F>(
    scanner: &Arc<dyn CableBleScanner>,
    eid_key: [u8; CABLE_EID_KEY_LEN],
    stop_on_first_match: bool,
    on_match: F,
    on_availability: AvailabilityCallback,
) -> Result<bool>
where
    F: Fn(MatchedCableEid) + Send + Sync + 'static,
{
    let matched_once = Arc::new(AtomicBool::new(false));
    let scanner_weak = Arc::downgrade(scanner);
    let on_match = Arc::new(on_match);
    let on_advert: AdvertCallback = Arc::new(move |service_data: Vec<u8>| {
        if stop_on_first_match && matched_once.load(Ordering::SeqCst) {
            return;
        }
        let Some(matched) = match_cable_service_data_bytes(&service_data, &eid_key, None) else {
            return;
        };
        if stop_on_first_match {
            if matched_once.swap(true, Ordering::SeqCst) {
                return;
            }
            if let Some(strong) = scanner_weak.upgrade() {
                strong.stop();
            }
        }
        on_match(matched);
    });
    scanner.start(on_advert, on_availability)
}

// ============================================================================
// Deterministic Fake Scanner
// ============================================================================

#[derive(Default)]
struct FakeScannerState {
    bluetooth_available: bool,
    auto_notify_availability: bool,
    running: bool,
    error_message: Option<String>,
    on_advert: Option<AdvertCallback>,
    on_availability: Option<AvailabilityCallback>,
    seen: BTreeSet<Vec<u8>>,
}

/// Deterministic in-memory [`CableBleScanner`] for unit and integration tests.
///
/// Never touches real Bluetooth hardware; callers inject synthetic BLE advertisements via
/// [`Self::emit_advertisement`] or [`Self::emit_raw_advert`].
#[derive(Clone)]
pub struct FakeCableBleScanner {
    inner: Arc<Mutex<FakeScannerState>>,
    start_calls: Arc<AtomicUsize>,
    stop_calls: Arc<AtomicUsize>,
}

impl Default for FakeCableBleScanner {
    fn default() -> Self {
        Self::new(true)
    }
}

impl FakeCableBleScanner {
    /// Creates a new fake scanner with `bluetooth_available` determining whether `start` succeeds.
    #[must_use]
    pub fn new(bluetooth_available: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeScannerState {
                bluetooth_available,
                auto_notify_availability: true,
                running: false,
                error_message: None,
                on_advert: None,
                on_availability: None,
                seen: BTreeSet::new(),
            })),
            start_calls: Arc::new(AtomicUsize::new(0)),
            stop_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Configures whether `start` automatically invokes `on_availability(true)` (default `true`).
    pub fn set_auto_notify_availability(&self, enabled: bool) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.auto_notify_availability = enabled;
        }
    }

    /// Updates simulated Bluetooth radio presence for subsequent [`CableBleScanner::start`] calls.
    pub fn set_bluetooth_available(&self, available: bool) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.bluetooth_available = available;
        }
    }

    /// Injects an error returned by subsequent [`CableBleScanner::start`] calls.
    pub fn set_error(&self, message: Option<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.error_message = message;
        }
    }

    /// Delivers a synthetic availability notification while the scanner is running.
    pub fn emit_availability(&self, available: bool) -> bool {
        let cb = {
            let Ok(guard) = self.inner.lock() else {
                return false;
            };
            if !guard.running {
                return false;
            }
            guard.on_availability.clone()
        };
        if let Some(callback) = cb {
            callback(available);
            true
        } else {
            false
        }
    }

    /// Injects a raw caBLE service-data payload (already stripped of the 2-byte UUID).
    ///
    /// Mirrors `WinBleScanner` deduplication (`state->seen.emplace(blob).second`): returns `true`
    /// only if the scanner is running and this exact payload has not been delivered yet.
    pub fn emit_raw_advert(&self, service_data: Vec<u8>) -> bool {
        let cb = {
            let Ok(mut guard) = self.inner.lock() else {
                return false;
            };
            if !guard.running {
                return false;
            }
            if !guard.seen.insert(service_data.clone()) {
                return false;
            }
            guard.on_advert.clone()
        };
        if let Some(callback) = cb {
            callback(service_data);
            true
        } else {
            false
        }
    }

    /// Parses a full synthetic [`BleAdvertisement`] (filtering out non-caBLE / malformed sections)
    /// and delivers every new caBLE service-data payload to the active `on_advert` callback.
    ///
    /// Returns the number of newly delivered payloads.
    pub fn emit_advertisement(&self, advertisement: &BleAdvertisement) -> usize {
        let extracted = extract_cable_service_data(advertisement);
        let mut delivered = 0;
        for item in extracted {
            if self.emit_raw_advert(item.service_data) {
                delivered += 1;
            }
        }
        delivered
    }

    /// Parses raw BLE GAP AD bytes (`[len, type, payload...]`) and injects any valid caBLE
    /// payloads found inside.
    pub fn emit_raw_ble_packet(&self, raw_packet: &[u8]) -> usize {
        let adv = BleAdvertisement {
            sections: parse_raw_ble_ad_packet(raw_packet),
            bluetooth_address: None,
            rssi: None,
        };
        self.emit_advertisement(&adv)
    }

    /// Number of times [`CableBleScanner::start`] was called.
    #[must_use]
    pub fn start_call_count(&self) -> usize {
        self.start_calls.load(Ordering::SeqCst)
    }

    /// Number of times [`CableBleScanner::stop`] was called.
    #[must_use]
    pub fn stop_call_count(&self) -> usize {
        self.stop_calls.load(Ordering::SeqCst)
    }

    /// Snapshot of all unique caBLE adverts seen during the current or most recent run.
    #[must_use]
    pub fn seen_adverts(&self) -> Vec<Vec<u8>> {
        self.inner
            .lock()
            .map(|g| g.seen.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl CableBleScanner for FakeCableBleScanner {
    fn start(
        &self,
        on_advert: AdvertCallback,
        on_availability: AvailabilityCallback,
    ) -> Result<bool> {
        self.start_calls.fetch_add(1, Ordering::SeqCst);
        let notify = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| PlatformError::CableScanner("fake scanner lock poisoned".into()))?;
            if let Some(ref msg) = guard.error_message {
                return Err(PlatformError::CableScanner(msg.clone()));
            }
            if !guard.bluetooth_available {
                guard.running = false;
                guard.on_advert = None;
                guard.on_availability = None;
                return Ok(false);
            }
            guard.running = true;
            guard.seen.clear();
            guard.on_advert = Some(on_advert);
            guard.on_availability = Some(Arc::clone(&on_availability));
            guard.auto_notify_availability
        };
        if notify {
            on_availability(true);
        }
        Ok(true)
    }

    fn stop(&self) {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.inner.lock() {
            guard.running = false;
            guard.on_advert = None;
            guard.on_availability = None;
        }
    }

    fn is_running(&self) -> bool {
        self.inner.lock().map(|g| g.running).unwrap_or(false)
    }
}

// ============================================================================
// Production Windows WinRT BLE Advertisement Watcher
// ============================================================================

/// Production Windows BLE scanner backed by WinRT `BluetoothLEAdvertisementWatcher` and
/// `bthprops.cpl` radio presence checks (`WinBleScanner` in `cable_scanner_win.cpp:107-197`).
pub struct WinRtCableBleScanner {
    #[cfg(windows)]
    state: Mutex<Option<winrt_impl::ActiveWatcher>>,
    #[cfg(not(windows))]
    running: AtomicBool,
}

impl Default for WinRtCableBleScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl WinRtCableBleScanner {
    /// Creates a new idle [`WinRtCableBleScanner`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            state: Mutex::new(None),
            #[cfg(not(windows))]
            running: AtomicBool::new(false),
        }
    }

    /// Probes whether at least one local Bluetooth radio is present via `bthprops.cpl`.
    #[must_use]
    pub fn bluetooth_radio_present() -> bool {
        #[cfg(windows)]
        {
            winrt_impl::bluetooth_radio_present()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

impl Drop for WinRtCableBleScanner {
    fn drop(&mut self) {
        self.stop();
    }
}

impl CableBleScanner for WinRtCableBleScanner {
    fn start(
        &self,
        on_advert: AdvertCallback,
        on_availability: AvailabilityCallback,
    ) -> Result<bool> {
        #[cfg(windows)]
        {
            self.stop();
            if !winrt_impl::bluetooth_radio_present() {
                return Ok(false);
            }
            let active = winrt_impl::start_watcher(on_advert)?;
            {
                let mut guard = self
                    .state
                    .lock()
                    .map_err(|_| PlatformError::CableScanner("watcher lock poisoned".into()))?;
                *guard = Some(active);
            }
            on_availability(true);
            Ok(true)
        }
        #[cfg(not(windows))]
        {
            let _ = (on_advert, on_availability);
            self.running.store(false, Ordering::SeqCst);
            Ok(false)
        }
    }

    fn stop(&self) {
        #[cfg(windows)]
        {
            if let Ok(mut guard) = self.state.lock()
                && let Some(active) = guard.take()
            {
                active.stop();
            }
        }
        #[cfg(not(windows))]
        {
            self.running.store(false, Ordering::SeqCst);
        }
    }

    fn is_running(&self) -> bool {
        #[cfg(windows)]
        {
            self.state.lock().map(|g| g.is_some()).unwrap_or(false)
        }
        #[cfg(not(windows))]
        {
            self.running.load(Ordering::SeqCst)
        }
    }
}

#[cfg(windows)]
mod winrt_impl {
    use std::collections::BTreeSet;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use windows::Devices::Bluetooth::Advertisement::{
        BluetoothLEAdvertisement, BluetoothLEAdvertisementReceivedEventArgs,
        BluetoothLEAdvertisementWatcher, BluetoothLEScanningMode,
    };
    use windows::Foundation::TypedEventHandler;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::Devices::Bluetooth::BLUETOOTH_FIND_RADIO_PARAMS;
    use windows::Win32::Foundation::{CloseHandle, FreeLibrary, HANDLE};
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows_core::{BOOL, PCSTR, PCWSTR, w};

    use super::{
        AdvertCallback, BLE_AD_TYPE_SERVICE_DATA_16, CABLE_SERVICE_DATA_MIN_LEN,
        parse_cable_service_data_section,
    };
    use crate::error::{PlatformError, Result};

    type FnBluetoothFindFirstRadio =
        unsafe extern "system" fn(*const BLUETOOTH_FIND_RADIO_PARAMS, *mut HANDLE) -> *mut c_void;
    type FnBluetoothFindRadioClose = unsafe extern "system" fn(*mut c_void) -> BOOL;

    /// Port of `BluetoothRadioPresent()` in `cable_scanner_win.cpp:83-98`.
    pub(super) fn bluetooth_radio_present() -> bool {
        // SAFETY: Loading system DLL `bthprops.cpl` exclusively from System32.
        let Ok(module) =
            (unsafe { LoadLibraryExW(w!("bthprops.cpl"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) })
        else {
            return false;
        };

        struct ModuleGuard(windows::Win32::Foundation::HMODULE);
        impl Drop for ModuleGuard {
            fn drop(&mut self) {
                // SAFETY: Releasing our own library handle once.
                let _ = unsafe { FreeLibrary(self.0) };
            }
        }
        let _guard = ModuleGuard(module);

        // SAFETY: Resolving standard Bluetooth API exports by exact ASCII name.
        let find_first_raw =
            unsafe { GetProcAddress(module, PCSTR(c"BluetoothFindFirstRadio".as_ptr().cast())) };
        let find_close_raw =
            unsafe { GetProcAddress(module, PCSTR(c"BluetoothFindRadioClose".as_ptr().cast())) };
        let (Some(find_first_ptr), Some(find_close_ptr)) = (find_first_raw, find_close_raw) else {
            return false;
        };

        // SAFETY: Signatures match `<bluetoothapis.h>` stdcall declarations.
        let find_first: FnBluetoothFindFirstRadio = unsafe { std::mem::transmute(find_first_ptr) };
        let find_close: FnBluetoothFindRadioClose = unsafe { std::mem::transmute(find_close_ptr) };

        let params = BLUETOOTH_FIND_RADIO_PARAMS {
            dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
        };
        let mut radio = HANDLE::default();
        // SAFETY: `params` is properly sized and `radio` receives an owned radio handle on success.
        let find_handle = unsafe { find_first(&params, &mut radio) };
        if find_handle.is_null() {
            return false;
        }
        if !radio.is_invalid() {
            // SAFETY: Closing the radio handle returned by `BluetoothFindFirstRadio`.
            let _ = unsafe { CloseHandle(radio) };
        }
        // SAFETY: Closing the enumeration handle returned by `BluetoothFindFirstRadio`.
        let _ = unsafe { find_close(find_handle) };
        true
    }

    fn extract_from_winrt_advertisement(advert: &BluetoothLEAdvertisement) -> Vec<Vec<u8>> {
        let Ok(sections) = advert.GetSectionsByType(BLE_AD_TYPE_SERVICE_DATA_16) else {
            return Vec::new();
        };
        let mut results = Vec::new();
        for section in sections {
            let Ok(buffer) = section.Data() else {
                continue;
            };
            let Ok(len) = buffer.Length() else {
                continue;
            };
            if (len as usize) < CABLE_SERVICE_DATA_MIN_LEN {
                continue;
            }
            let Ok(reader) = DataReader::FromBuffer(&buffer) else {
                continue;
            };
            let mut bytes = vec![0u8; len as usize];
            if reader.ReadBytes(&mut bytes).is_err() {
                continue;
            }
            if let Some(parsed) =
                parse_cable_service_data_section(BLE_AD_TYPE_SERVICE_DATA_16, &bytes)
            {
                results.push(parsed.service_data);
            }
        }
        results
    }

    struct WatcherSharedState {
        stopped: AtomicBool,
        seen: Mutex<BTreeSet<Vec<u8>>>,
        on_advert: AdvertCallback,
    }

    pub(super) struct ActiveWatcher {
        watcher: BluetoothLEAdvertisementWatcher,
        token: i64,
        shared: Arc<WatcherSharedState>,
    }

    impl ActiveWatcher {
        pub(super) fn stop(self) {
            self.shared.stopped.store(true, Ordering::SeqCst);
            let _ = self.watcher.RemoveReceived(self.token);
            let _ = self.watcher.Stop();
        }
    }

    pub(super) fn start_watcher(on_advert: AdvertCallback) -> Result<ActiveWatcher> {
        // Ensure WinRT is initialized on the calling thread (ignoring RPC_E_CHANGED_MODE if STA).
        // SAFETY: `RoInitialize` is safe to invoke; if the apartment is already initialized in STA
        // it returns `RPC_E_CHANGED_MODE` which is harmless for creating a WinRT agile watcher.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

        let watcher = BluetoothLEAdvertisementWatcher::new()
            .map_err(|e| PlatformError::from_win("BluetoothLEAdvertisementWatcher::new", e))?;
        watcher
            .SetScanningMode(BluetoothLEScanningMode::Active)
            .map_err(|e| {
                PlatformError::from_win("BluetoothLEAdvertisementWatcher::SetScanningMode", e)
            })?;

        let shared = Arc::new(WatcherSharedState {
            stopped: AtomicBool::new(false),
            seen: Mutex::new(BTreeSet::new()),
            on_advert,
        });
        let handler_state = Arc::clone(&shared);
        let handler = TypedEventHandler::<
            BluetoothLEAdvertisementWatcher,
            BluetoothLEAdvertisementReceivedEventArgs,
        >::new(move |_sender, args| {
            if handler_state.stopped.load(Ordering::SeqCst) {
                return Ok(());
            }
            let Some(args) = args.as_ref() else {
                return Ok(());
            };
            let Ok(advertisement) = args.Advertisement() else {
                return Ok(());
            };
            let blobs = extract_from_winrt_advertisement(&advertisement);
            for blob in blobs {
                if handler_state.stopped.load(Ordering::SeqCst) {
                    break;
                }
                let is_new = handler_state
                    .seen
                    .lock()
                    .map(|mut seen| seen.insert(blob.clone()))
                    .unwrap_or(false);
                if is_new {
                    (handler_state.on_advert)(blob);
                }
            }
            Ok(())
        });

        let token = watcher
            .Received(&handler)
            .map_err(|e| PlatformError::from_win("BluetoothLEAdvertisementWatcher::Received", e))?;
        if let Err(err) = watcher.Start() {
            let _ = watcher.RemoveReceived(token);
            return Err(PlatformError::from_win(
                "BluetoothLEAdvertisementWatcher::Start",
                err,
            ));
        }

        // Reference `PCWSTR` only if needed; silence unused import warning cleanly.
        let _ = PCWSTR::null();

        Ok(ActiveWatcher {
            watcher,
            token,
            shared,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webauthn::{CableDerivedValueType, cable_derive, cable_encrypt_advert};

    fn sample_eid_and_key() -> (
        [u8; CABLE_EID_KEY_LEN],
        [u8; CABLE_EID_LEN],
        [u8; CABLE_ADVERT_LEN],
    ) {
        let secret = [0x42u8; 16];
        let eid_key: [u8; CABLE_EID_KEY_LEN] =
            cable_derive(&secret, &[], CableDerivedValueType::EidKey);
        let mut eid = [0u8; CABLE_EID_LEN];
        eid[0] = 0; // reserved zero byte
        eid[1..11].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]); // 10-byte nonce
        eid[11..14].copy_from_slice(&[0xAA, 0xBB, 0xCC]); // 3-byte routing ID
        eid[14..16].copy_from_slice(&0u16.to_le_bytes()); // domain 0 => "cable.ua5v.com"
        let advert = cable_encrypt_advert(&eid, &eid_key);
        (eid_key, eid, advert)
    }

    #[test]
    fn parses_fido_and_google_service_data_and_rejects_unrelated() {
        let (eid_key, eid, advert) = sample_eid_and_key();

        let fido_section = build_cable_service_data_section(CableServiceUuid::Fido, &advert);
        let parsed =
            parse_cable_service_data_section(fido_section.data_type, &fido_section.data).unwrap();
        assert_eq!(parsed.uuid, CableServiceUuid::Fido);
        assert_eq!(parsed.advert_bytes(), Some(&advert));
        let matched = match_cable_service_data(&parsed, &eid_key).unwrap();
        assert_eq!(matched.decrypted_eid, eid);
        assert_eq!(matched.tunnel_domain, "cable.ua5v.com");

        let google_section = build_cable_service_data_section(CableServiceUuid::Google, &advert);
        let parsed_g =
            parse_cable_service_data_section(google_section.data_type, &google_section.data)
                .unwrap();
        assert_eq!(parsed_g.uuid, CableServiceUuid::Google);

        // Wrong AD type
        assert!(parse_cable_service_data_section(0xFF, &fido_section.data).is_none());
        // Unrelated UUID (0x180F Battery Service)
        let mut bad_uuid = vec![0x0F, 0x18];
        bad_uuid.extend_from_slice(&advert);
        assert!(parse_cable_service_data_section(BLE_AD_TYPE_SERVICE_DATA_16, &bad_uuid).is_none());
        // Too short (19-byte advert)
        assert!(
            parse_cable_service_data_section(BLE_AD_TYPE_SERVICE_DATA_16, &fido_section.data[..21])
                .is_none()
        );
    }

    #[test]
    fn raw_ble_packet_parser_handles_malformed_and_valid_sections() {
        let (_eid_key, _eid, advert) = sample_eid_and_key();
        let mut pkt = Vec::new();
        // Flags section: len=2, type=0x01, data=0x06
        pkt.extend_from_slice(&[2, 0x01, 0x06]);
        // FIDO Service Data section: len=1+2+20=23, type=0x16, uuid=0xFFF9 LE, advert
        pkt.push(23);
        pkt.push(BLE_AD_TYPE_SERVICE_DATA_16);
        pkt.extend_from_slice(&CABLE_FIDO_UUID16.to_le_bytes());
        pkt.extend_from_slice(&advert);
        // Trailing truncated section should be ignored cleanly
        pkt.extend_from_slice(&[10, 0xFF, 0x01]);

        let sections = parse_raw_ble_ad_packet(&pkt);
        assert_eq!(sections.len(), 2);
        let extracted = extract_cable_service_data(&BleAdvertisement {
            sections,
            bluetooth_address: None,
            rssi: Some(-55),
        });
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].uuid, CableServiceUuid::Fido);
        assert_eq!(extracted[0].service_data, advert);
    }

    #[test]
    fn fake_scanner_deduplicates_and_stops_on_first_match() {
        let (eid_key, eid, advert) = sample_eid_and_key();
        let fake = FakeCableBleScanner::new(true);
        let scanner: Arc<dyn CableBleScanner> = Arc::new(fake.clone());

        let avail = Arc::new(Mutex::new(Vec::new()));
        let avail_clone = Arc::clone(&avail);
        let matches = Arc::new(Mutex::new(Vec::new()));
        let matches_clone = Arc::clone(&matches);

        let started = start_cable_proximity_match(
            &scanner,
            eid_key,
            true,
            move |m| matches_clone.lock().unwrap().push(m),
            Arc::new(move |ok| avail_clone.lock().unwrap().push(ok)),
        )
        .unwrap();
        assert!(started);
        assert!(fake.is_running());
        assert_eq!(*avail.lock().unwrap(), vec![true]);

        // Unrelated advertisement is ignored
        let unrelated = BleAdvertisement {
            sections: vec![BleDataSection {
                data_type: 0x09,
                data: b"OtherDevice".to_vec(),
            }],
            ..Default::default()
        };
        assert_eq!(fake.emit_advertisement(&unrelated), 0);
        assert!(matches.lock().unwrap().is_empty());

        // Valid caBLE advertisement matches and stops scanner
        let valid_adv = BleAdvertisement {
            sections: vec![build_cable_service_data_section(
                CableServiceUuid::Fido,
                &advert,
            )],
            ..Default::default()
        };
        assert_eq!(fake.emit_advertisement(&valid_adv), 1);
        assert!(!fake.is_running());
        assert_eq!(fake.stop_call_count(), 1);

        // Subsequent emission after stop is ignored
        assert_eq!(fake.emit_advertisement(&valid_adv), 0);
        let matched = matches.lock().unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].decrypted_eid, eid);
    }
}
