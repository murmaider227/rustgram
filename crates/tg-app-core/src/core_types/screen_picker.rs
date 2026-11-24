//! Screen-sharing source enumeration (Windows monitors and visible windows).

use tg_voip::list_displays;

/// Whether a screen-sharing source represents an entire display or an application window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenSourceKind {
    /// Physical or virtual monitor / desktop display.
    Display,
    /// Top-level visible application window.
    Window,
}

/// Selectable screen-sharing capture source returned by [`enumerate_screen_sources`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenShareSource {
    /// Stable source identifier (e.g. `"display:\\\\.\\DISPLAY1"` or `"window:0x1A2B"`).
    pub id: String,
    /// Human-readable label shown in the picker UI.
    pub name: String,
    /// Monitor vs window kind.
    pub kind: ScreenSourceKind,
    /// Source width in pixels (0 if unknown).
    pub width: u32,
    /// Source height in pixels (0 if unknown).
    pub height: u32,
    /// Target DXGI display identifier passed to `tg_voip::ScreenCaptureConfig::display_id`.
    /// For a window source, this is the monitor containing the window.
    pub display_id: String,
    /// Raw Win32 `HWND` value for window sources (`None` for displays).
    pub hwnd: Option<usize>,
}

impl ScreenShareSource {
    /// Returns a formatted resolution caption such as `"1920×1080"`, or empty if unknown.
    #[must_use]
    pub fn resolution_label(&self) -> String {
        if self.width > 0 && self.height > 0 {
            format!("{}×{}", self.width, self.height)
        } else {
            String::new()
        }
    }
}

/// Enumerates shareable displays and visible top-level windows on the current machine.
///
/// On Windows, displays are queried through `tg_voip::list_displays()` (DXGI) with a Win32
/// `EnumDisplayMonitors` fallback, and visible titled windows are enumerated via
/// `EnumWindows` + `GetWindowTextW` + `MonitorFromWindow`.
#[must_use]
pub fn enumerate_screen_sources() -> Vec<ScreenShareSource> {
    let mut sources = Vec::new();

    #[cfg(windows)]
    {
        enumerate_windows_displays(&mut sources);
        enumerate_windows_top_level(&mut sources);
    }

    if !sources.iter().any(|s| s.kind == ScreenSourceKind::Display) {
        sources.insert(
            0,
            ScreenShareSource {
                id: "display:default".to_owned(),
                name: "Primary Display".to_owned(),
                kind: ScreenSourceKind::Display,
                width: 1920,
                height: 1080,
                display_id: String::new(),
                hwnd: None,
            },
        );
    }

    sources
}

#[cfg(windows)]
fn enumerate_windows_displays(out: &mut Vec<ScreenShareSource>) {
    if let Ok(displays) = list_displays()
        && !displays.is_empty()
    {
        for (idx, d) in displays.into_iter().enumerate() {
            let display_name = if d.name.trim().is_empty() {
                format!("Display {}", idx + 1)
            } else {
                d.name
            };
            out.push(ScreenShareSource {
                id: format!("display:{}", d.id),
                name: display_name,
                kind: ScreenSourceKind::Display,
                width: d.width,
                height: d.height,
                display_id: d.id,
                hwnd: None,
            });
        }
        return;
    }

    // Fallback to Win32 GDI EnumDisplayMonitors if DXGI enumeration returned empty/error.
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };
    use windows::core::BOOL;

    unsafe extern "system" fn monitor_enum_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` carries a valid `&mut Vec<ScreenShareSource>` pointer from
        // `enumerate_windows_displays` for the synchronous duration of `EnumDisplayMonitors`.
        let list = unsafe { &mut *(lparam.0 as *mut Vec<ScreenShareSource>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` is stack-allocated with `cbSize` properly initialized.
        let ok =
            unsafe { GetMonitorInfoW(hmonitor, std::ptr::addr_of_mut!(info).cast()).as_bool() };
        if ok {
            let raw_name: Vec<u16> = info
                .szDevice
                .iter()
                .copied()
                .take_while(|&c| c != 0)
                .collect();
            let device_id = String::from_utf16_lossy(&raw_name);
            let rc = info.monitorInfo.rcMonitor;
            let width = (rc.right - rc.left).max(0) as u32;
            let height = (rc.bottom - rc.top).max(0) as u32;
            let idx = list.len() + 1;
            let label = if device_id.is_empty() {
                format!("Display {idx}")
            } else {
                format!("Display {idx} ({device_id})")
            };
            list.push(ScreenShareSource {
                id: format!("display:{device_id}"),
                name: label,
                kind: ScreenSourceKind::Display,
                width,
                height,
                display_id: device_id,
                hwnd: None,
            });
        }
        BOOL(1)
    }

    let mut monitors: Vec<ScreenShareSource> = Vec::new();
    // SAFETY: `monitor_enum_proc` is a valid `MONITORENUMPROC` and `&mut monitors` outlives
    // the synchronous `EnumDisplayMonitors` call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_enum_proc),
            LPARAM(std::ptr::addr_of_mut!(monitors) as isize),
        );
    }
    out.extend(monitors);
}

#[cfg(windows)]
fn enumerate_windows_top_level(out: &mut Vec<ScreenShareSource>) {
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic, IsWindowVisible,
    };
    use windows::core::BOOL;

    struct EnumCtx {
        windows: Vec<ScreenShareSource>,
        fallback_display_id: String,
    }

    unsafe extern "system" fn window_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` points to `EnumCtx` owned by `enumerate_windows_top_level` for the
        // synchronous duration of `EnumWindows`.
        let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx) };
        if ctx.windows.len() >= 48 {
            return BOOL(0);
        }
        // SAFETY: `hwnd` is supplied by the OS during `EnumWindows`.
        let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
        if !visible {
            return BOOL(1);
        }
        // SAFETY: `hwnd` is a valid top-level window handle.
        if unsafe { IsIconic(hwnd).as_bool() } {
            return BOOL(1);
        }
        // SAFETY: `hwnd` is valid; returns UTF-16 character count excluding NUL.
        let text_len = unsafe { GetWindowTextLengthW(hwnd) };
        if text_len <= 0 {
            return BOOL(1);
        }
        let mut buf = vec![0u16; (text_len as usize).saturating_add(2)];
        // SAFETY: `buf` is writable with capacity `buf.len()`.
        let written = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if written <= 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buf[..written as usize])
            .trim()
            .to_owned();
        if title.is_empty() || title == "Program Manager" {
            return BOOL(1);
        }

        let mut rect = RECT::default();
        // SAFETY: `rect` is stack-allocated and valid for `GetWindowRect`.
        let has_rect = unsafe { GetWindowRect(hwnd, &mut rect).is_ok() };
        let (width, height) = if has_rect {
            (
                (rect.right - rect.left).max(0) as u32,
                (rect.bottom - rect.top).max(0) as u32,
            )
        } else {
            (0, 0)
        };
        if width < 64 || height < 64 {
            return BOOL(1);
        }

        let mut display_id = ctx.fallback_display_id.clone();
        // SAFETY: `hwnd` is a valid window handle; `MonitorFromWindow` returns nearest `HMONITOR`.
        let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if !hmon.is_invalid() {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            // SAFETY: `info` is stack-allocated with `cbSize` set.
            if unsafe { GetMonitorInfoW(hmon, std::ptr::addr_of_mut!(info).cast()).as_bool() } {
                let raw_name: Vec<u16> = info
                    .szDevice
                    .iter()
                    .copied()
                    .take_while(|&c| c != 0)
                    .collect();
                let dev = String::from_utf16_lossy(&raw_name);
                if !dev.is_empty() {
                    display_id = dev;
                }
            }
        }

        let raw_hwnd = hwnd.0 as usize;
        ctx.windows.push(ScreenShareSource {
            id: format!("window:0x{raw_hwnd:x}"),
            name: title,
            kind: ScreenSourceKind::Window,
            width,
            height,
            display_id,
            hwnd: Some(raw_hwnd),
        });
        BOOL(1)
    }

    let fallback_display_id = out
        .iter()
        .find(|s| s.kind == ScreenSourceKind::Display)
        .map_or_else(String::new, |s| s.display_id.clone());
    let mut ctx = EnumCtx {
        windows: Vec::new(),
        fallback_display_id,
    };
    // SAFETY: `window_enum_proc` is a valid `WNDENUMPROC` and `&mut ctx` outlives the
    // synchronous `EnumWindows` call.
    unsafe {
        let _ = EnumWindows(
            Some(window_enum_proc),
            LPARAM(std::ptr::addr_of_mut!(ctx) as isize),
        );
    }
    out.extend(ctx.windows);
}
