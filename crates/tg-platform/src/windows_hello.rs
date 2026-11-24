//! Windows Hello biometric / PIN user consent verification via WinRT
//! `Windows.Security.Credentials.UI.UserConsentVerifier`, behind a testable
//! [`WindowsHelloVerifier`] trait with a deterministic [`FakeWindowsHelloVerifier`].
//!
//! Port of:
//! - `Telegram/lib_base/base/system_unlock.h`
//! - `Telegram/lib_base/base/platform/win/base_system_unlock_win.h`
//! - `Telegram/lib_base/base/platform/win/base_system_unlock_win.cpp`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::Result;

/// Availability reported by WinRT `UserConsentVerifier::CheckAvailabilityAsync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowsHelloAvailability {
    /// Windows Hello verification device is ready to prompt the user.
    Available,
    /// No biometric/PIN verifier device is present.
    DeviceNotPresent,
    /// Windows Hello is not configured for the current Windows user account.
    NotConfiguredForUser,
    /// Windows Hello is disabled by enterprise / Group Policy.
    DisabledByPolicy,
    /// Verification hardware is currently busy.
    DeviceBusy,
    /// Platform or WinRT runtime does not support Windows Hello.
    #[default]
    Unavailable,
}

impl WindowsHelloAvailability {
    /// Returns `true` when Windows Hello can currently verify the user.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Outcome of a Windows Hello verification prompt (`UserConsentVerificationResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowsHelloResult {
    /// User successfully authenticated via Windows Hello.
    Verified,
    /// No verifier device is present.
    DeviceNotPresent,
    /// Windows Hello is not configured for the current user.
    NotConfiguredForUser,
    /// Verification is disabled by Group Policy.
    DisabledByPolicy,
    /// Verification device is busy.
    DeviceBusy,
    /// User exhausted allowed authentication retries (maps to `SystemUnlockResult::FloodError`).
    RetriesExhausted,
    /// User dismissed or cancelled the consent prompt (maps to `SystemUnlockResult::Cancelled`).
    #[default]
    Canceled,
}

impl WindowsHelloResult {
    /// Returns `true` when verification succeeded.
    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, Self::Verified)
    }

    /// Returns `true` when retries were exhausted (tdesktop flood-error path).
    #[must_use]
    pub const fn is_flood_error(self) -> bool {
        matches!(self, Self::RetriesExhausted)
    }
}

/// Abstraction over Windows Hello `UserConsentVerifier` so UI and tests can swap in
/// [`FakeWindowsHelloVerifier`] without ever popping a native OS consent dialog.
pub trait WindowsHelloVerifier: Send + Sync {
    /// Checks whether Windows Hello is configured and available (`CheckAvailabilityAsync`).
    fn check_availability(&self) -> Result<WindowsHelloAvailability>;

    /// Prompts the user to verify their identity for `hwnd` with display `message`
    /// (`IUserConsentVerifierInterop::RequestVerificationForWindowAsync`).
    fn request_verification(&self, hwnd: isize, message: &str) -> Result<WindowsHelloResult>;
}

/// Production Windows WinRT `UserConsentVerifier` backend.
///
/// Executes WinRT calls on a dedicated MTA worker thread so calling from an STA GUI thread never
/// deadlocks while waiting on `IAsyncOperation<...>`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinRtWindowsHelloVerifier;

impl WinRtWindowsHelloVerifier {
    /// Creates a new [`WinRtWindowsHelloVerifier`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl WindowsHelloVerifier for WinRtWindowsHelloVerifier {
    fn check_availability(&self) -> Result<WindowsHelloAvailability> {
        #[cfg(windows)]
        {
            winrt_impl::check_availability()
        }
        #[cfg(not(windows))]
        {
            Ok(WindowsHelloAvailability::Unavailable)
        }
    }

    fn request_verification(&self, hwnd: isize, message: &str) -> Result<WindowsHelloResult> {
        #[cfg(windows)]
        {
            winrt_impl::request_verification(hwnd, message)
        }
        #[cfg(not(windows))]
        {
            let _ = (hwnd, message);
            Ok(WindowsHelloResult::Canceled)
        }
    }
}

/// Deterministic in-memory [`WindowsHelloVerifier`] for tests and headless environments.
#[derive(Debug, Clone)]
pub struct FakeWindowsHelloVerifier {
    inner: Arc<Mutex<FakeHelloState>>,
    availability_calls: Arc<AtomicUsize>,
    verification_calls: Arc<AtomicUsize>,
}

#[derive(Debug, Clone)]
struct FakeHelloState {
    availability: WindowsHelloAvailability,
    result: WindowsHelloResult,
    error_message: Option<String>,
    requests: Vec<(isize, String)>,
}

impl Default for FakeWindowsHelloVerifier {
    fn default() -> Self {
        Self::available_verified()
    }
}

impl FakeWindowsHelloVerifier {
    /// Creates a fake verifier that reports [`WindowsHelloAvailability::Available`] and
    /// authenticates with [`WindowsHelloResult::Verified`].
    #[must_use]
    pub fn available_verified() -> Self {
        Self::with_outcome(
            WindowsHelloAvailability::Available,
            WindowsHelloResult::Verified,
        )
    }

    /// Creates a fake verifier returning explicit `availability` and `result`.
    #[must_use]
    pub fn with_outcome(
        availability: WindowsHelloAvailability,
        result: WindowsHelloResult,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeHelloState {
                availability,
                result,
                error_message: None,
                requests: Vec::new(),
            })),
            availability_calls: Arc::new(AtomicUsize::new(0)),
            verification_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Creates a fake verifier simulating an unavailable / unconfigured Windows Hello device.
    #[must_use]
    pub fn unavailable(availability: WindowsHelloAvailability) -> Self {
        Self::with_outcome(availability, WindowsHelloResult::NotConfiguredForUser)
    }

    /// Updates the availability returned by subsequent [`Self::check_availability`] calls.
    pub fn set_availability(&self, availability: WindowsHelloAvailability) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.availability = availability;
        }
    }

    /// Updates the result returned by subsequent [`Self::request_verification`] calls.
    pub fn set_result(&self, result: WindowsHelloResult) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.result = result;
        }
    }

    /// Injects an error to be returned by subsequent calls.
    pub fn set_error(&self, message: Option<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.error_message = message;
        }
    }

    /// Number of times [`WindowsHelloVerifier::check_availability`] was called.
    #[must_use]
    pub fn availability_call_count(&self) -> usize {
        self.availability_calls.load(Ordering::SeqCst)
    }

    /// Number of times [`WindowsHelloVerifier::request_verification`] was called.
    #[must_use]
    pub fn verification_call_count(&self) -> usize {
        self.verification_calls.load(Ordering::SeqCst)
    }

    /// Returns all `(hwnd, message)` requests recorded so far.
    #[must_use]
    pub fn recorded_requests(&self) -> Vec<(isize, String)> {
        self.inner
            .lock()
            .map(|g| g.requests.clone())
            .unwrap_or_default()
    }
}

impl WindowsHelloVerifier for FakeWindowsHelloVerifier {
    fn check_availability(&self) -> Result<WindowsHelloAvailability> {
        self.availability_calls.fetch_add(1, Ordering::SeqCst);
        let guard = self
            .inner
            .lock()
            .map_err(|_| crate::error::PlatformError::WindowsHello("lock poisoned".to_owned()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(crate::error::PlatformError::WindowsHello(msg.clone()));
        }
        Ok(guard.availability)
    }

    fn request_verification(&self, hwnd: isize, message: &str) -> Result<WindowsHelloResult> {
        self.verification_calls.fetch_add(1, Ordering::SeqCst);
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| crate::error::PlatformError::WindowsHello("lock poisoned".to_owned()))?;
        guard.requests.push((hwnd, message.to_owned()));
        if let Some(ref msg) = guard.error_message {
            return Err(crate::error::PlatformError::WindowsHello(msg.clone()));
        }
        if !guard.availability.is_available() {
            return Ok(match guard.availability {
                WindowsHelloAvailability::DeviceNotPresent => WindowsHelloResult::DeviceNotPresent,
                WindowsHelloAvailability::NotConfiguredForUser => {
                    WindowsHelloResult::NotConfiguredForUser
                }
                WindowsHelloAvailability::DisabledByPolicy => WindowsHelloResult::DisabledByPolicy,
                WindowsHelloAvailability::DeviceBusy => WindowsHelloResult::DeviceBusy,
                WindowsHelloAvailability::Unavailable => WindowsHelloResult::Canceled,
                WindowsHelloAvailability::Available => guard.result,
            });
        }
        Ok(guard.result)
    }
}

#[cfg(windows)]
mod winrt_impl {
    use std::ffi::c_void;

    use windows::Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::{
        IUserConsentVerifierInterop, RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
    };
    use windows_core::HSTRING;

    use super::{WindowsHelloAvailability, WindowsHelloResult};
    use crate::error::{PlatformError, Result};

    struct RoInitGuard(bool);

    impl RoInitGuard {
        fn enter_mta() -> Self {
            // SAFETY: Initializes WinRT MTA apartment on a dedicated worker thread; paired with
            // `RoUninitialize` in `Drop` only if initialization succeeded.
            let ok = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
            Self(ok)
        }
    }

    impl Drop for RoInitGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Matches successful `RoInitialize` on the same thread.
                unsafe { RoUninitialize() };
            }
        }
    }

    fn map_availability(avail: UserConsentVerifierAvailability) -> WindowsHelloAvailability {
        match avail {
            UserConsentVerifierAvailability::Available => WindowsHelloAvailability::Available,
            UserConsentVerifierAvailability::DeviceNotPresent => {
                WindowsHelloAvailability::DeviceNotPresent
            }
            UserConsentVerifierAvailability::NotConfiguredForUser => {
                WindowsHelloAvailability::NotConfiguredForUser
            }
            UserConsentVerifierAvailability::DisabledByPolicy => {
                WindowsHelloAvailability::DisabledByPolicy
            }
            UserConsentVerifierAvailability::DeviceBusy => WindowsHelloAvailability::DeviceBusy,
            _ => WindowsHelloAvailability::Unavailable,
        }
    }

    fn map_result(res: UserConsentVerificationResult) -> WindowsHelloResult {
        match res {
            UserConsentVerificationResult::Verified => WindowsHelloResult::Verified,
            UserConsentVerificationResult::DeviceNotPresent => WindowsHelloResult::DeviceNotPresent,
            UserConsentVerificationResult::NotConfiguredForUser => {
                WindowsHelloResult::NotConfiguredForUser
            }
            UserConsentVerificationResult::DisabledByPolicy => WindowsHelloResult::DisabledByPolicy,
            UserConsentVerificationResult::DeviceBusy => WindowsHelloResult::DeviceBusy,
            UserConsentVerificationResult::RetriesExhausted => WindowsHelloResult::RetriesExhausted,
            _ => WindowsHelloResult::Canceled,
        }
    }

    pub(super) fn check_availability() -> Result<WindowsHelloAvailability> {
        let handle = std::thread::Builder::new()
            .name("tg-winrt-hello-check".to_owned())
            .spawn(|| -> Result<WindowsHelloAvailability> {
                let _guard = RoInitGuard::enter_mta();
                let op = UserConsentVerifier::CheckAvailabilityAsync().map_err(|e| {
                    PlatformError::from_win("UserConsentVerifier::CheckAvailabilityAsync", e)
                })?;
                let avail = op
                    .join()
                    .map_err(|e| PlatformError::from_win("CheckAvailabilityAsync::join", e))?;
                Ok(map_availability(avail))
            })
            .map_err(PlatformError::Io)?;

        handle.join().map_err(|_| {
            PlatformError::WindowsHello("Windows Hello availability thread panicked".to_owned())
        })?
    }

    pub(super) fn request_verification(hwnd: isize, message: &str) -> Result<WindowsHelloResult> {
        let msg_owned = message.to_owned();
        let handle = std::thread::Builder::new()
            .name("tg-winrt-hello-verify".to_owned())
            .spawn(move || -> Result<WindowsHelloResult> {
                let _guard = RoInitGuard::enter_mta();
                let hmsg = HSTRING::from(&msg_owned);
                let op = if hwnd != 0 {
                    let interop =
                        windows_core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
                            .map_err(|e| {
                                PlatformError::from_win(
                                    "factory<UserConsentVerifier, IUserConsentVerifierInterop>",
                                    e,
                                )
                            })?;
                    // SAFETY: `hwnd` is caller-provided top-level HWND and `hmsg` is a valid
                    // WinRT HSTRING alive across the COM invocation.
                    unsafe {
                        interop.RequestVerificationForWindowAsync(HWND(hwnd as *mut c_void), &hmsg)
                    }
                    .map_err(|e| {
                        PlatformError::from_win(
                            "IUserConsentVerifierInterop::RequestVerificationForWindowAsync",
                            e,
                        )
                    })?
                } else {
                    UserConsentVerifier::RequestVerificationAsync(&hmsg).map_err(|e| {
                        PlatformError::from_win("UserConsentVerifier::RequestVerificationAsync", e)
                    })?
                };
                let res = op
                    .join()
                    .map_err(|e| PlatformError::from_win("RequestVerificationAsync::join", e))?;
                Ok(map_result(res))
            })
            .map_err(PlatformError::Io)?;

        handle.join().map_err(|_| {
            PlatformError::WindowsHello("Windows Hello verification thread panicked".to_owned())
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_verifier_records_availability_and_requests() {
        let fake = FakeWindowsHelloVerifier::available_verified();
        assert_eq!(
            fake.check_availability().expect("availability"),
            WindowsHelloAvailability::Available
        );
        let res = fake
            .request_verification(0x1234, "Unlock Rustgram")
            .expect("verify");
        assert!(res.is_verified());
        assert_eq!(fake.availability_call_count(), 1);
        assert_eq!(fake.verification_call_count(), 1);
        assert_eq!(
            fake.recorded_requests(),
            vec![(0x1234, "Unlock Rustgram".to_owned())]
        );
    }

    #[test]
    fn fake_verifier_handles_retries_exhausted_unavailable_and_errors() {
        let fake = FakeWindowsHelloVerifier::with_outcome(
            WindowsHelloAvailability::Available,
            WindowsHelloResult::RetriesExhausted,
        );
        let res = fake.request_verification(42, "Unlock").expect("verify");
        assert!(res.is_flood_error());
        assert!(!res.is_verified());

        fake.set_availability(WindowsHelloAvailability::NotConfiguredForUser);
        assert_eq!(
            fake.request_verification(42, "Unlock").expect("verify"),
            WindowsHelloResult::NotConfiguredForUser
        );

        fake.set_error(Some("consent service unavailable".to_owned()));
        let err = fake.request_verification(42, "Unlock").unwrap_err();
        assert!(err.to_string().contains("consent service unavailable"));
    }
}
