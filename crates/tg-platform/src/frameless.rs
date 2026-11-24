//! Win32 custom frameless title bar, Aero snap styles, and DWM window attributes.
//!
//! Port of the Win32 main-window frame integration from
//! `Telegram/SourceFiles/platform/win/main_window_win.{h,cpp}`:
//! - Dark-mode title bar synchronization (`DWMWA_USE_IMMERSIVE_DARK_MODE`)
//! - Frameless Aero snap style & 1px DWM shadow margin (`WS_THICKFRAME | WS_CAPTION`,
//!   `DwmExtendFrameIntoClientArea`, `DWMWA_WINDOW_CORNER_PREFERENCE`)
//! - Passcode-locked iconic thumbnail attribute (`DWMWA_FORCE_ICONIC_REPRESENTATION` /
//!   `DWMWA_HAS_ICONIC_BITMAP`)
//! - Border / title-bar hit-testing geometry for custom frameless eframe viewports.

#[cfg(windows)]
use crate::error::PlatformError;
use crate::error::Result;

/// Resize border thickness in logical pixels for custom frameless windows.
pub const DEFAULT_RESIZE_BORDER_PX: f32 = 6.0;

/// Default custom title bar height in logical pixels.
pub const DEFAULT_TITLE_BAR_HEIGHT_PX: f32 = 28.0;

/// Hit-test zone for a custom frameless window frame (maps to `WM_NCHITTEST` / egui resize drag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramelessHitZone {
    /// Inside normal client content.
    Client,
    /// Inside the draggable title bar caption region.
    Caption,
    /// Window resize border.
    Resize(ResizeEdge),
}

/// Edge or corner of a frameless window resize drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    /// Top edge.
    North,
    /// Bottom edge.
    South,
    /// Left edge.
    West,
    /// Right edge.
    East,
    /// Top-left corner.
    NorthWest,
    /// Top-right corner.
    NorthEast,
    /// Bottom-left corner.
    SouthWest,
    /// Bottom-right corner.
    SouthEast,
}

/// Computes which frameless hit-test zone a pointer at `(x, y)` inside `(0..width, 0..height)` hits.
///
/// `controls_right_width` reserves the right-side minimize/maximize/close buttons inside the title
/// bar so clicking them stays [`FramelessHitZone::Client`] instead of triggering a window drag.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn hit_test_frameless(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    maximized: bool,
    border: f32,
    title_height: f32,
    controls_right_width: f32,
) -> FramelessHitZone {
    if !maximized && border > 0.0 {
        let left = x < border;
        let right = x >= (width - border);
        let top = y < border;
        let bottom = y >= (height - border);
        match (left, right, top, bottom) {
            (true, _, true, _) => return FramelessHitZone::Resize(ResizeEdge::NorthWest),
            (_, true, true, _) => return FramelessHitZone::Resize(ResizeEdge::NorthEast),
            (true, _, _, true) => return FramelessHitZone::Resize(ResizeEdge::SouthWest),
            (_, true, _, true) => return FramelessHitZone::Resize(ResizeEdge::SouthEast),
            (true, _, _, _) => return FramelessHitZone::Resize(ResizeEdge::West),
            (_, true, _, _) => return FramelessHitZone::Resize(ResizeEdge::East),
            (_, _, true, _) => return FramelessHitZone::Resize(ResizeEdge::North),
            (_, _, _, true) => return FramelessHitZone::Resize(ResizeEdge::South),
            _ => {}
        }
    }

    if y >= 0.0 && y < title_height && x >= 0.0 && x < (width - controls_right_width) {
        return FramelessHitZone::Caption;
    }

    FramelessHitZone::Client
}

/// Trait abstracting Win32 DWM/frame calls for headless testing.
pub trait WindowFrameBackend {
    /// Synchronizes Win32 immersive dark mode (`DWMWA_USE_IMMERSIVE_DARK_MODE`) for `hwnd`.
    fn set_dark_mode(&mut self, hwnd: isize, dark: bool) -> Result<()>;

    /// Enables or disables Aero-snap-compatible borderless styles and 1px DWM drop shadow.
    fn set_frameless_aero_snap(&mut self, hwnd: isize, enabled: bool) -> Result<()>;

    /// Toggles `DWMWA_FORCE_ICONIC_REPRESENTATION` / `DWMWA_HAS_ICONIC_BITMAP` when passcode-locked.
    fn set_passcode_iconic_preview(&mut self, hwnd: isize, locked: bool) -> Result<()>;
}

/// Real Windows Win32/DWM window frame backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32WindowFrameBackend;

impl WindowFrameBackend for Win32WindowFrameBackend {
    fn set_dark_mode(&mut self, hwnd: isize, dark: bool) -> Result<()> {
        apply_window_dark_mode(hwnd, dark)
    }

    fn set_frameless_aero_snap(&mut self, hwnd: isize, enabled: bool) -> Result<()> {
        apply_frameless_aero_snap(hwnd, enabled)
    }

    fn set_passcode_iconic_preview(&mut self, hwnd: isize, locked: bool) -> Result<()> {
        apply_dwm_passcode_iconic(hwnd, locked)
    }
}

/// Deterministic in-memory [`WindowFrameBackend`] for headless unit & UI tests.
#[derive(Debug, Clone, Default)]
pub struct FakeWindowFrameBackend {
    /// Recorded `(hwnd, dark)` calls to [`WindowFrameBackend::set_dark_mode`].
    pub dark_mode_calls: Vec<(isize, bool)>,
    /// Recorded `(hwnd, enabled)` calls to [`WindowFrameBackend::set_frameless_aero_snap`].
    pub aero_snap_calls: Vec<(isize, bool)>,
    /// Recorded `(hwnd, locked)` calls to [`WindowFrameBackend::set_passcode_iconic_preview`].
    pub passcode_iconic_calls: Vec<(isize, bool)>,
}

impl FakeWindowFrameBackend {
    /// Creates an empty fake window-frame backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl WindowFrameBackend for FakeWindowFrameBackend {
    fn set_dark_mode(&mut self, hwnd: isize, dark: bool) -> Result<()> {
        self.dark_mode_calls.push((hwnd, dark));
        Ok(())
    }

    fn set_frameless_aero_snap(&mut self, hwnd: isize, enabled: bool) -> Result<()> {
        self.aero_snap_calls.push((hwnd, enabled));
        Ok(())
    }

    fn set_passcode_iconic_preview(&mut self, hwnd: isize, locked: bool) -> Result<()> {
        self.passcode_iconic_calls.push((hwnd, locked));
        Ok(())
    }
}

/// Applies `DWMWA_USE_IMMERSIVE_DARK_MODE` to `hwnd` (`main_window_win.cpp:714-737`).
///
/// Returns `Ok(())` when `hwnd == 0` or on non-Windows platforms.
pub fn apply_window_dark_mode(hwnd: isize, dark: bool) -> Result<()> {
    if hwnd == 0 {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};

        let val: i32 = i32::from(dark);
        // SAFETY: `hwnd` is a valid top-level window handle passed by the caller and `val` is a
        // 4-byte `BOOL` matching `DWMWA_USE_IMMERSIVE_DARK_MODE`.
        let hr = unsafe {
            DwmSetWindowAttribute(
                HWND(hwnd as *mut c_void),
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&raw const val).cast::<c_void>(),
                std::mem::size_of::<i32>() as u32,
            )
        };
        hr.map_err(|e| PlatformError::from_win("DwmSetWindowAttribute(DARK_MODE)", e))?;
    }
    #[cfg(not(windows))]
    let _ = dark;
    Ok(())
}

/// Configures Win32 style bits & DWM frame extension so a borderless (`with_decorations(false)`)
/// eframe/winit window still receives Windows Aero Snap and a subtle DWM shadow.
///
/// Returns `Ok(())` when `hwnd == 0` or on non-Windows platforms.
pub fn apply_frameless_aero_snap(hwnd: isize, enabled: bool) -> Result<()> {
    if hwnd == 0 {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
            DwmSetWindowAttribute,
        };
        use windows::Win32::UI::Controls::MARGINS;
        use windows::Win32::UI::WindowsAndMessaging::{
            GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
            WS_THICKFRAME,
        };

        let handle = HWND(hwnd as *mut c_void);
        // SAFETY: `handle` is caller-provided top-level HWND. Adding `WS_THICKFRAME |
        // WS_MAXIMIZEBOX | WS_MINIMIZEBOX` preserves Aero Snap on undecorated winit windows.
        unsafe {
            let current = GetWindowLongPtrW(handle, GWL_STYLE);
            let snap_bits = (WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0 | WS_MINIMIZEBOX.0) as isize;
            let updated = if enabled {
                current | snap_bits
            } else {
                current
            };
            if updated != current {
                SetWindowLongPtrW(handle, GWL_STYLE, updated);
                let _ = SetWindowPos(
                    handle,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }

            let margins = if enabled {
                MARGINS {
                    cxLeftWidth: 0,
                    cxRightWidth: 0,
                    cyTopHeight: 1,
                    cyBottomHeight: 0,
                }
            } else {
                MARGINS::default()
            };
            let _ = DwmExtendFrameIntoClientArea(handle, &margins);

            if enabled {
                let corner = DWMWCP_ROUND.0;
                let _ = DwmSetWindowAttribute(
                    handle,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    (&raw const corner).cast::<c_void>(),
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
    }
    #[cfg(not(windows))]
    let _ = enabled;
    Ok(())
}

/// Applies `DWMWA_FORCE_ICONIC_REPRESENTATION` and `DWMWA_HAS_ICONIC_BITMAP` when passcode-locked
/// (`MainWindow::setupPreviewPasscodeLock` in `main_window_win.cpp:402-419`).
pub fn apply_dwm_passcode_iconic(hwnd: isize, locked: bool) -> Result<()> {
    if hwnd == 0 {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{
            DWMWA_FORCE_ICONIC_REPRESENTATION, DWMWA_HAS_ICONIC_BITMAP, DwmSetWindowAttribute,
        };

        let handle = HWND(hwnd as *mut c_void);
        let val: i32 = i32::from(locked);
        // SAFETY: `handle` is a top-level HWND and `val` is a 4-byte `BOOL`.
        unsafe {
            DwmSetWindowAttribute(
                handle,
                DWMWA_FORCE_ICONIC_REPRESENTATION,
                (&raw const val).cast::<c_void>(),
                std::mem::size_of::<i32>() as u32,
            )
            .map_err(|e| PlatformError::from_win("DwmSetWindowAttribute(FORCE_ICONIC)", e))?;
            DwmSetWindowAttribute(
                handle,
                DWMWA_HAS_ICONIC_BITMAP,
                (&raw const val).cast::<c_void>(),
                std::mem::size_of::<i32>() as u32,
            )
            .map_err(|e| PlatformError::from_win("DwmSetWindowAttribute(HAS_ICONIC_BITMAP)", e))?;
        }
    }
    #[cfg(not(windows))]
    let _ = locked;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_corners_edges_caption_and_controls() {
        let w = 1000.0;
        let h = 700.0;
        let border = DEFAULT_RESIZE_BORDER_PX;
        let title = DEFAULT_TITLE_BAR_HEIGHT_PX;
        let controls = 138.0;

        assert_eq!(
            hit_test_frameless(2.0, 2.0, w, h, false, border, title, controls),
            FramelessHitZone::Resize(ResizeEdge::NorthWest)
        );
        assert_eq!(
            hit_test_frameless(997.0, 697.0, w, h, false, border, title, controls),
            FramelessHitZone::Resize(ResizeEdge::SouthEast)
        );
        assert_eq!(
            hit_test_frameless(500.0, 2.0, w, h, false, border, title, controls),
            FramelessHitZone::Resize(ResizeEdge::North)
        );
        // Maximized disables resize borders so top edge becomes draggable caption.
        assert_eq!(
            hit_test_frameless(500.0, 2.0, w, h, true, border, title, controls),
            FramelessHitZone::Caption
        );
        // Caption drag area vs right window buttons.
        assert_eq!(
            hit_test_frameless(200.0, 14.0, w, h, false, border, title, controls),
            FramelessHitZone::Caption
        );
        assert_eq!(
            hit_test_frameless(950.0, 14.0, w, h, false, border, title, controls),
            FramelessHitZone::Client
        );
        // Normal client content.
        assert_eq!(
            hit_test_frameless(300.0, 100.0, w, h, false, border, title, controls),
            FramelessHitZone::Client
        );
    }

    #[test]
    fn zero_hwnd_is_noop() {
        assert!(apply_window_dark_mode(0, true).is_ok());
        assert!(apply_frameless_aero_snap(0, true).is_ok());
        assert!(apply_dwm_passcode_iconic(0, true).is_ok());
    }
}
