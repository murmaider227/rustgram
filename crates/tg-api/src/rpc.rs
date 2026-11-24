//! Generic MTProto RPC client trait and error types for `tg-api`.
//!
//! Port origin: `mtproto/sender.h`, `mtproto/mtproto_response.h`.
//!
//! Defines a `tg_tl::RemoteCall`-generic [`RpcClient`] trait shaped identically to
//! `tg_mtproto::RpcClient` so wiring the real MTProto transport is a single-line impl.

use std::future::Future;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::broadcast;

/// Telegram data center identifier (e.g. `1..=5`).
pub type DcId = i32;

/// Telegram MTProto RPC error (`MTP::Error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    /// Numeric HTTP-like error code (e.g. `303`, `400`, `401`, `406`, `420`, `500`).
    pub code: i32,
    /// Uppercase Telegram error identifier (e.g. `"PHONE_NUMBER_INVALID"`, `"FLOOD_WAIT_30"`).
    pub name: String,
    /// Optional numeric suffix parsed from the error string (e.g. `Some(30)` for `FLOOD_WAIT_30`).
    pub value: Option<u32>,
}

impl RpcError {
    /// Creates a new `RpcError`, automatically parsing a trailing `_<number>` suffix if present.
    #[must_use]
    pub fn new(code: i32, name: impl Into<String>) -> Self {
        let name = name.into();
        let value = name
            .rsplit_once('_')
            .and_then(|(_, suffix)| suffix.parse::<u32>().ok());
        Self { code, name, value }
    }

    /// Returns `true` if this is a flood wait error (`MTP::IsFloodError`).
    #[must_use]
    pub fn is_flood_error(&self) -> bool {
        self.name.starts_with("FLOOD_WAIT_") || self.name.starts_with("FLOOD_PREMIUM_WAIT_")
    }

    /// Extracts the wait duration in seconds for `FLOOD_WAIT_X` / `FLOOD_PREMIUM_WAIT_X`.
    #[must_use]
    pub fn flood_wait_seconds(&self) -> Option<u32> {
        if self.is_flood_error() {
            self.value
        } else {
            None
        }
    }

    /// Extracts the target DC ID for `PHONE_MIGRATE_X`, `USER_MIGRATE_X`,
    /// `NETWORK_MIGRATE_X`, or `FILE_MIGRATE_X`.
    #[must_use]
    pub fn migrate_dc(&self) -> Option<DcId> {
        for prefix in [
            "PHONE_MIGRATE_",
            "USER_MIGRATE_",
            "NETWORK_MIGRATE_",
            "FILE_MIGRATE_",
        ] {
            if let Some(rest) = self.name.strip_prefix(prefix)
                && let Ok(dc) = rest.parse::<DcId>()
                && dc > 0
            {
                return Some(dc);
            }
        }
        None
    }

    /// Extracts the wait seconds from `SLOWMODE_WAIT_X`.
    #[must_use]
    pub fn slowmode_wait_seconds(&self) -> Option<u32> {
        self.name
            .strip_prefix("SLOWMODE_WAIT_")
            .and_then(|s| s.parse::<u32>().ok())
    }

    /// Extracts the wait seconds from `2FA_CONFIRM_WAIT_X`.
    #[must_use]
    pub fn confirm_wait_seconds(&self) -> Option<u32> {
        self.name
            .strip_prefix("2FA_CONFIRM_WAIT_")
            .and_then(|s| s.parse::<u32>().ok())
    }

    /// Extracts required stars from `ALLOW_PAYMENT_REQUIRED_X`.
    #[must_use]
    pub fn allow_payment_required_stars(&self) -> Option<u32> {
        self.name
            .strip_prefix("ALLOW_PAYMENT_REQUIRED_")
            .and_then(|s| s.parse::<u32>().ok())
    }

    /// Returns `true` if the error should be silently ignored (`MTP::IgnoreError`: code `406`).
    #[must_use]
    pub const fn is_ignored(&self) -> bool {
        self.code == 406
    }

    /// Returns `true` if the error is temporary (`MTP::IsTemporaryError`).
    #[must_use]
    pub fn is_temporary(&self) -> bool {
        self.code < 0 || self.code >= 500 || self.is_flood_error()
    }

    /// Returns `true` if this is a `400 FILE_REFERENCE_*` error (`FILE_REFERENCE_EXPIRED`,
    /// `FILE_REFERENCE_INVALID`, `FILE_REFERENCE_0_EXPIRED`, etc., matching
    /// `DownloadMtprotoTask::normalPartFailed` in `storage/download_manager_mtproto.cpp:936-937`).
    #[must_use]
    pub fn is_file_reference_error(&self) -> bool {
        self.code == 400 && self.name.starts_with("FILE_REFERENCE_")
    }

    /// Extracts `<part>` from a `FILE_PART_<part>_MISSING` RPC error, if present.
    #[must_use]
    pub fn missing_file_part(&self) -> Option<u32> {
        let rest = self.name.strip_prefix("FILE_PART_")?;
        let digits = rest.strip_suffix("_MISSING")?;
        digits.parse::<u32>().ok()
    }
}

/// MTProto RPC invocation error.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InvocationError {
    /// Server returned an RPC error.
    #[error("RPC error {0:?}")]
    Rpc(RpcError),
    /// Request was dropped before receiving a response.
    #[error("request dropped")]
    Dropped,
    /// Underlying transport error.
    #[error("transport error: {0}")]
    Transport(String),
    /// Failed to deserialize TL response.
    #[error("deserialize error: {0}")]
    Deserialize(String),
    /// Request was cancelled locally.
    #[error("request cancelled")]
    Cancelled,
}

impl From<RpcError> for InvocationError {
    fn from(err: RpcError) -> Self {
        Self::Rpc(err)
    }
}

/// Generic MTProto RPC client trait parameterized over generated `tg_tl::RemoteCall` functions.
///
/// Mirrors `tg_mtproto::RpcClient` (`async fn invoke<R: tg_tl::RemoteCall>(&self, req: &R)`)
/// with DC-routing and updates stream hooks so `tg-mtproto` can wire in with a one-line impl.
pub trait RpcClient: Send + Sync {
    /// Error type returned by this client, convertible into [`InvocationError`].
    type Error: Into<InvocationError> + Send;

    /// Invokes a generated TL RPC function `R` on the current main DC.
    fn invoke<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send;

    /// Invokes a generated TL RPC function `R` on a specific data center (`dc_id`).
    ///
    /// Defaults to forwarding to [`Self::invoke`].
    fn invoke_in_dc<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        self.invoke(req)
    }

    /// Invokes `R` on the main DC with `handle_flood_errors` enabled so `FLOOD_WAIT_X` returns
    /// directly to the caller (matching `.handleFloodErrors()` in tdesktop's `intro/` widgets:
    /// `intro/intro_phone.cpp`, `intro/intro_code.cpp`, `intro/intro_password_check.cpp`,
    /// `intro/intro_signup.cpp`, `intro/intro_qr.cpp`).
    ///
    /// Defaults to forwarding to [`Self::invoke`].
    fn invoke_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        self.invoke(req)
    }

    /// Invokes `R` on `dc_id` with `handle_flood_errors` enabled
    /// (`.toDC(dcId).handleFloodErrors()` in `intro/intro_qr.cpp`).
    ///
    /// Defaults to forwarding to [`Self::invoke_in_dc`].
    fn invoke_in_dc_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        self.invoke_in_dc(dc_id, req)
    }

    /// Returns the currently selected main data center ID (default: DC 2).
    fn main_dc_id(&self) -> DcId {
        2
    }

    /// Switches the main data center ID (e.g. on `PHONE_MIGRATE_X` or `loginTokenMigrateTo`).
    fn set_main_dc_id(&self, _dc_id: DcId) {}

    /// Subscribes to the live `tg_tl::enums::Updates` push stream from MTProto, if supported.
    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tg_tl::enums::Updates>> {
        None
    }

    /// Sends a keepalive/liveness ping to the main DC (`MTP::Instance::ping`).
    ///
    /// Default implementation is a no-op returning `Ok(())`.
    fn ping(&self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async { Ok(()) }
    }

    /// Restarts active transport sessions (e.g. after network interface change or sleep).
    ///
    /// Default implementation is a no-op.
    fn restart_sessions(&self) {}
}

impl<C: RpcClient + ?Sized> RpcClient for &C {
    type Error = C::Error;

    fn invoke<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke(req)
    }

    fn invoke_in_dc<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke_in_dc(dc_id, req)
    }

    fn invoke_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke_handle_flood(req)
    }

    fn invoke_in_dc_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke_in_dc_handle_flood(dc_id, req)
    }

    fn main_dc_id(&self) -> DcId {
        (**self).main_dc_id()
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        (**self).set_main_dc_id(dc_id);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tg_tl::enums::Updates>> {
        (**self).subscribe_updates()
    }

    fn ping(&self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        (**self).ping()
    }

    fn restart_sessions(&self) {
        (**self).restart_sessions();
    }
}

impl<C: RpcClient + ?Sized> RpcClient for Arc<C> {
    type Error = C::Error;

    fn invoke<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke(req)
    }

    fn invoke_in_dc<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke_in_dc(dc_id, req)
    }

    fn invoke_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke_handle_flood(req)
    }

    fn invoke_in_dc_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, Self::Error>> + Send {
        (**self).invoke_in_dc_handle_flood(dc_id, req)
    }

    fn main_dc_id(&self) -> DcId {
        (**self).main_dc_id()
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        (**self).set_main_dc_id(dc_id);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tg_tl::enums::Updates>> {
        (**self).subscribe_updates()
    }

    fn ping(&self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        (**self).ping()
    }

    fn restart_sessions(&self) {
        (**self).restart_sessions();
    }
}
