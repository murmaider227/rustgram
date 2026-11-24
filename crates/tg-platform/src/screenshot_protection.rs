//! Window screenshot / screen-capture protection coordinator.
//!
//! Port of:
//! - `Telegram/SourceFiles/core/core_screenshot_protection.h`
//! - `Telegram/SourceFiles/core/core_screenshot_protection.cpp`
//! - `Telegram/SourceFiles/platform/win/specific_win.cpp`
//!   (`ScreenshotProtectionSupported`, `AmbientScreenshotProtectionSupported`,
//!   `SetWindowScreenshotProtection`)
//!
//! Tracks multiple independent protection reasons (e.g. a protected chat with `noforwards`,
//! a media viewer displaying protected content, or a user screenshot-blocking setting) per
//! window handle, applying `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` with graceful
//! fallback to `WDA_MONITOR` on older Windows builds (< Windows 10 2004).

use std::collections::BTreeMap;

use crate::error::Result;

/// Opaque identifier for a registered screenshot-protection reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReasonId(u64);

/// Classification of a screenshot-protection reason, mirroring `Core::ScreenshotProtection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionScope {
    /// The window itself directly displays protected content (e.g. media viewer, protected chat).
    Content,
    /// Surrounding ambient context is protected; on Windows tdesktop skips ambient reasons
    /// when `AmbientScreenshotProtectionSupported()` is false unless explicitly forced.
    Ambient,
}

/// Returns `true` when window screenshot protection is supported by the platform.
///
/// Mirrors `Platform::ScreenshotProtectionSupported()` in `specific_win.cpp:521-523`.
#[must_use]
pub const fn screenshot_protection_supported() -> bool {
    cfg!(windows)
}

/// Returns `true` when ambient (context-only) screenshot protection is supported.
///
/// Mirrors `Platform::AmbientScreenshotProtectionSupported()` in `specific_win.cpp:525-528`:
/// Windows `SetWindowDisplayAffinity` hides the entire window from capture tools, so tdesktop
/// returns `false` for ambient-only reasons and uses `Content` reasons for protected views.
#[must_use]
pub const fn ambient_screenshot_protection_supported() -> bool {
    false
}

/// Applies or clears screenshot protection directly on a native window handle (`HWND` on Windows).
///
/// Uses `WDA_EXCLUDEFROMCAPTURE` (Windows 10 2004+) with automatic fallback to `WDA_MONITOR`
/// on older Windows versions, and `WDA_NONE` when `enabled == false`.
/// Returns `Ok(())` without side effects when `hwnd == 0`.
pub fn set_window_screenshot_protection(hwnd: isize, enabled: bool) -> Result<()> {
    if hwnd == 0 {
        return Ok(());
    }
    crate::system_info::set_window_screenshot_protection(hwnd, enabled)
}

/// Abstraction over OS display-affinity calls for headless unit testing.
pub trait DisplayAffinityBackend {
    /// Applies (`enabled = true`) or clears (`enabled = false`) screenshot protection for `hwnd`.
    fn set_window_protection(&mut self, hwnd: isize, enabled: bool) -> Result<()>;
}

/// Real OS backend delegating to [`set_window_screenshot_protection`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDisplayAffinity;

impl DisplayAffinityBackend for SystemDisplayAffinity {
    fn set_window_protection(&mut self, hwnd: isize, enabled: bool) -> Result<()> {
        set_window_screenshot_protection(hwnd, enabled)
    }
}

#[derive(Debug, Clone, Copy)]
struct Reason {
    hwnd: isize,
    active: bool,
}

/// Multi-reason per-window screenshot protection coordinator (`Core::ScreenshotProtection`).
///
/// Deduplicates OS calls so `SetWindowDisplayAffinity` is only invoked when the combined
/// protection state of a window changes (or when [`Self::reapply_window`] is called after
/// native handle recreation).
#[derive(Debug)]
pub struct ScreenshotProtectionManager<B: DisplayAffinityBackend = SystemDisplayAffinity> {
    backend: B,
    next_reason_id: u64,
    reasons: BTreeMap<ReasonId, Reason>,
    applied: BTreeMap<isize, bool>,
}

impl Default for ScreenshotProtectionManager<SystemDisplayAffinity> {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenshotProtectionManager<SystemDisplayAffinity> {
    /// Creates a manager wired to Windows `SetWindowDisplayAffinity`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(SystemDisplayAffinity)
    }
}

impl<B: DisplayAffinityBackend> ScreenshotProtectionManager<B> {
    /// Creates a manager with a custom [`DisplayAffinityBackend`] (useful in headless tests).
    #[must_use]
    pub fn with_backend(backend: B) -> Self {
        Self {
            backend,
            next_reason_id: 0,
            reasons: BTreeMap::new(),
            applied: BTreeMap::new(),
        }
    }

    /// Returns a shared reference to the underlying backend.
    #[must_use]
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Registers a content protection reason on `hwnd` (`Core::ScreenshotProtection::addContentReason`).
    pub fn add_content_reason(&mut self, hwnd: isize, active: bool) -> Result<ReasonId> {
        self.add_reason(hwnd, active)
    }

    /// Registers an ambient protection reason if supported on this platform
    /// (`Core::ScreenshotProtection::addAmbientReason`).
    pub fn add_ambient_reason(&mut self, hwnd: isize, active: bool) -> Result<Option<ReasonId>> {
        if !ambient_screenshot_protection_supported() {
            return Ok(None);
        }
        self.add_reason(hwnd, active).map(Some)
    }

    /// Registers a reason under explicit [`ProtectionScope`].
    pub fn add_scoped_reason(
        &mut self,
        scope: ProtectionScope,
        hwnd: isize,
        active: bool,
    ) -> Result<Option<ReasonId>> {
        match scope {
            ProtectionScope::Content => self.add_content_reason(hwnd, active).map(Some),
            ProtectionScope::Ambient => self.add_ambient_reason(hwnd, active),
        }
    }

    /// Updates whether `id` is currently active and synchronizes the OS display affinity.
    pub fn set_reason_active(&mut self, id: ReasonId, active: bool) -> Result<()> {
        let Some(reason) = self.reasons.get_mut(&id) else {
            return Ok(());
        };
        if reason.active == active {
            return Ok(());
        }
        let hwnd = reason.hwnd;
        reason.active = active;
        self.apply(hwnd, false)
    }

    /// Updates the native window handle associated with `id` (e.g. if a window is recreated).
    pub fn set_reason_window(&mut self, id: ReasonId, new_hwnd: isize) -> Result<()> {
        let Some(reason) = self.reasons.get_mut(&id) else {
            return Ok(());
        };
        let old_hwnd = reason.hwnd;
        if old_hwnd == new_hwnd {
            return Ok(());
        }
        reason.hwnd = new_hwnd;
        self.apply(old_hwnd, false)?;
        self.apply(new_hwnd, false)
    }

    /// Removes a previously registered reason and clears protection if no active reasons remain.
    pub fn remove_reason(&mut self, id: ReasonId) -> Result<()> {
        let Some(removed) = self.reasons.remove(&id) else {
            return Ok(());
        };
        self.apply(removed.hwnd, false)
    }

    /// Forces re-application of the current protection state for `hwnd` (mirrors `QEvent::WinIdChange`).
    pub fn reapply_window(&mut self, hwnd: isize) -> Result<()> {
        self.apply(hwnd, true)
    }

    /// Returns `true` if any active reason currently protects `hwnd`.
    #[must_use]
    pub fn is_protected(&self, hwnd: isize) -> bool {
        hwnd != 0 && self.reasons.values().any(|r| r.active && r.hwnd == hwnd)
    }

    fn add_reason(&mut self, hwnd: isize, active: bool) -> Result<ReasonId> {
        self.next_reason_id = self.next_reason_id.wrapping_add(1).max(1);
        let id = ReasonId(self.next_reason_id);
        self.reasons.insert(id, Reason { hwnd, active });
        self.apply(hwnd, false)?;
        Ok(id)
    }

    fn apply(&mut self, hwnd: isize, force: bool) -> Result<()> {
        if hwnd == 0 {
            return Ok(());
        }
        let enabled = self.is_protected(hwnd);
        if !force && self.applied.get(&hwnd).copied() == Some(enabled) {
            return Ok(());
        }
        self.backend.set_window_protection(hwnd, enabled)?;
        self.applied.insert(hwnd, enabled);
        Ok(())
    }
}

/// Evaluates whether a chat/viewer window requires screenshot protection based on
/// protected-content flags (`noforwards` / `forbids_saving`) or user settings.
#[must_use]
pub const fn should_protect_window(
    chat_forbids_saving: bool,
    media_forbids_saving: bool,
    user_block_screenshots_setting: bool,
) -> bool {
    chat_forbids_saving || media_forbids_saving || user_block_screenshots_setting
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingAffinity {
        calls: Vec<(isize, bool)>,
    }

    impl DisplayAffinityBackend for RecordingAffinity {
        fn set_window_protection(&mut self, hwnd: isize, enabled: bool) -> Result<()> {
            self.calls.push((hwnd, enabled));
            Ok(())
        }
    }

    #[test]
    fn multi_reason_refcounting_and_deduplication() {
        let mut mgr = ScreenshotProtectionManager::with_backend(RecordingAffinity::default());
        let hwnd = 0x1234_isize;

        let r1 = mgr.add_content_reason(hwnd, false).unwrap();
        assert!(!mgr.is_protected(hwnd));
        assert_eq!(mgr.backend().calls, vec![(hwnd, false)]);

        // First active reason enables protection.
        mgr.set_reason_active(r1, true).unwrap();
        assert!(mgr.is_protected(hwnd));
        assert_eq!(mgr.backend().calls, vec![(hwnd, false), (hwnd, true)]);

        // Adding a second active reason does NOT trigger a redundant OS call.
        let r2 = mgr.add_content_reason(hwnd, true).unwrap();
        assert_eq!(mgr.backend().calls.len(), 2);

        // Removing r1 keeps protection active because r2 is still active.
        mgr.remove_reason(r1).unwrap();
        assert!(mgr.is_protected(hwnd));
        assert_eq!(mgr.backend().calls.len(), 2);

        // Disabling r2 clears protection.
        mgr.set_reason_active(r2, false).unwrap();
        assert!(!mgr.is_protected(hwnd));
        assert_eq!(
            mgr.backend().calls,
            vec![(hwnd, false), (hwnd, true), (hwnd, false)]
        );

        // Forced reapply re-invokes backend even when state is unchanged.
        mgr.reapply_window(hwnd).unwrap();
        assert_eq!(mgr.backend().calls.len(), 4);
    }

    #[test]
    fn ambient_reason_respects_platform_support_and_zero_hwnd_is_noop() {
        let mut mgr = ScreenshotProtectionManager::with_backend(RecordingAffinity::default());
        assert_eq!(mgr.add_ambient_reason(0x55, true).unwrap(), None);
        assert!(mgr.backend().calls.is_empty());

        let _ = mgr.add_content_reason(0, true).unwrap();
        assert!(!mgr.is_protected(0));
        assert!(mgr.backend().calls.is_empty());

        assert!(should_protect_window(true, false, false));
        assert!(should_protect_window(false, true, false));
        assert!(should_protect_window(false, false, true));
        assert!(!should_protect_window(false, false, false));
    }
}
