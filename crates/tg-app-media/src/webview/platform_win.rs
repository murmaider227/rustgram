//! Windows Win32 helpers for WebView2 child HWND attachment, clipboard access, and capture.
//!
//! Port origin: `Telegram/lib_webview/webview/platform/win/webview_windows_edge_chromium.cpp`
//! and `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` (clipboard bridge).

use raw_window_handle::RawWindowHandle;

/// Finds a top-level window on the current UI thread whose title matches `title` exactly.
///
/// Used to obtain the Win32 `HWND` of an `eframe` secondary viewport so `tg_webview::WebView`
/// can embed a WebView2 child controller inside its client area.
#[must_use]
pub fn find_thread_window_by_title(title: &str) -> Option<isize> {
    #[cfg(windows)]
    {
        win_impl::find_thread_window_by_title(title)
    }
    #[cfg(not(windows))]
    {
        let _ = title;
        None
    }
}

/// Converts a non-zero Win32 `HWND` (`isize`) into a `raw-window-handle` [`RawWindowHandle`].
#[must_use]
pub fn win32_raw_handle(hwnd: isize) -> Option<RawWindowHandle> {
    #[cfg(windows)]
    {
        let nz = std::num::NonZeroIsize::new(hwnd)?;
        let handle = raw_window_handle::Win32WindowHandle::new(nz);
        Some(RawWindowHandle::Win32(handle))
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        None
    }
}

/// Queries the client area dimensions `(width, height)` in physical pixels for `hwnd`.
#[must_use]
pub fn client_size_for_hwnd(hwnd: isize) -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        win_impl::client_size_for_hwnd(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        None
    }
}

/// Reads plain UTF-8 text from the OS clipboard, returning `None` if unavailable or non-text.
#[must_use]
pub fn read_clipboard_text() -> Option<String> {
    #[cfg(windows)]
    {
        win_impl::read_clipboard_text()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Writes UTF-8 text to the OS clipboard, returning `true` on success.
pub fn write_clipboard_text(text: &str) -> bool {
    #[cfg(windows)]
    {
        win_impl::write_clipboard_text(text)
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        false
    }
}

/// Captures a window's full rendered contents (including DWM / WebView2 child HWNDs)
/// as top-down RGBA8 pixels `(width, height, rgba_bytes)`.
pub fn capture_hwnd_rgba(hwnd: isize) -> Result<(u32, u32, Vec<u8>), String> {
    #[cfg(windows)]
    {
        win_impl::capture_hwnd_rgba(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Err("Window capture is only supported on Windows".to_owned())
    }
}

#[cfg(windows)]
mod win_impl {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
    };
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumThreadWindows, GetClientRect, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        IsWindowVisible,
    };
    use windows::core::BOOL;

    const CF_UNICODETEXT_FMT: u32 = 13;
    const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrintWindow(
            hwnd: HWND,
            hdc_blt: windows::Win32::Graphics::Gdi::HDC,
            n_flags: u32,
        ) -> BOOL;
    }

    struct SearchState<'a> {
        target_title: &'a str,
        found: Option<isize>,
    }

    unsafe extern "system" fn enum_thread_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` points to a stack-pinned `SearchState` alive for the synchronous
        // duration of `EnumThreadWindows`.
        let state = unsafe { &mut *(lparam.0 as *mut SearchState<'_>) };
        // SAFETY: `hwnd` is a valid window handle supplied by `EnumThreadWindows`.
        let len = unsafe { GetWindowTextLengthW(hwnd) };
        if len <= 0 {
            return BOOL(1);
        }
        let mut buf = vec![0u16; (len as usize) + 1];
        // SAFETY: `buf` is large enough for `len + 1` UTF-16 code units.
        let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if copied > 0 {
            let text = String::from_utf16_lossy(&buf[..copied as usize]);
            if text == state.target_title {
                // Prefer visible windows, but remember first exact match.
                // SAFETY: `hwnd` is valid.
                let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
                if visible || state.found.is_none() {
                    state.found = Some(hwnd.0 as isize);
                    if visible {
                        return BOOL(0);
                    }
                }
            }
        }
        BOOL(1)
    }

    pub(super) fn find_thread_window_by_title(title: &str) -> Option<isize> {
        if title.is_empty() {
            return None;
        }
        let mut state = SearchState {
            target_title: title,
            found: None,
        };
        // SAFETY: `GetCurrentThreadId` and `EnumThreadWindows` run synchronously on the current
        // thread, and `state` outlives the enumeration call.
        unsafe {
            let tid = GetCurrentThreadId();
            let _ = EnumThreadWindows(
                tid,
                Some(enum_thread_proc),
                LPARAM(&mut state as *mut SearchState<'_> as isize),
            );
        }
        state.found
    }

    pub(super) fn client_size_for_hwnd(hwnd: isize) -> Option<(i32, i32)> {
        if hwnd == 0 {
            return None;
        }
        let mut rc = RECT::default();
        // SAFETY: `hwnd` is checked non-zero and `rc` is a valid out-pointer.
        let ok = unsafe { GetClientRect(HWND(hwnd as *mut c_void), &mut rc) }.is_ok();
        if ok {
            Some(((rc.right - rc.left).max(0), (rc.bottom - rc.top).max(0)))
        } else {
            None
        }
    }

    pub(super) fn read_clipboard_text() -> Option<String> {
        // SAFETY: Standard Win32 clipboard sequence (`OpenClipboard` -> `GetClipboardData` ->
        // `GlobalLock`/`GlobalUnlock` -> `CloseClipboard`).
        unsafe {
            if OpenClipboard(None).is_err() {
                return None;
            }
            let result = (|| {
                let handle = GetClipboardData(CF_UNICODETEXT_FMT).ok()?;
                let hglobal = windows::Win32::Foundation::HGLOBAL(handle.0);
                let ptr = GlobalLock(hglobal) as *const u16;
                if ptr.is_null() {
                    return None;
                }
                let mut len = 0usize;
                // Cap read length at 1 MiB of UTF-16 code units for safety.
                while len < 1_048_576 && *ptr.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(ptr, len);
                let text = String::from_utf16_lossy(slice);
                let _ = GlobalUnlock(hglobal);
                Some(text)
            })();
            let _ = CloseClipboard();
            result
        }
    }

    pub(super) fn write_clipboard_text(text: &str) -> bool {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = wide.len() * std::mem::size_of::<u16>();

        // SAFETY: Standard Win32 clipboard write sequence allocating `GMEM_MOVEABLE` memory
        // whose ownership transfers to the OS once `SetClipboardData` succeeds.
        unsafe {
            if OpenClipboard(None).is_err() {
                return false;
            }
            let ok = (|| {
                if EmptyClipboard().is_err() {
                    return false;
                }
                let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, byte_len) else {
                    return false;
                };
                let dst = GlobalLock(hmem) as *mut u16;
                if dst.is_null() {
                    return false;
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                let _ = GlobalUnlock(hmem);
                let handle = windows::Win32::Foundation::HANDLE(hmem.0);
                SetClipboardData(CF_UNICODETEXT_FMT, Some(handle)).is_ok()
            })();
            let _ = CloseClipboard();
            ok
        }
    }

    pub(super) fn capture_hwnd_rgba(hwnd_raw: isize) -> Result<(u32, u32, Vec<u8>), String> {
        if hwnd_raw == 0 {
            return Err("HWND is null".to_owned());
        }
        let hwnd = HWND(hwnd_raw as *mut c_void);
        let mut rect = RECT::default();
        // SAFETY: `hwnd` is non-null and `rect` is a valid out-pointer.
        unsafe { GetWindowRect(hwnd, &mut rect) }
            .map_err(|e| format!("GetWindowRect failed: {e}"))?;
        let width = (rect.right - rect.left).max(1);
        let height = (rect.bottom - rect.top).max(1);

        // SAFETY: Allocates a compatible GDI memory DC and DIB bitmap, invokes `PrintWindow`
        // with `PW_RENDERFULLCONTENT`, extracts BGRA pixels via `GetDIBits`, and frees all GDI
        // resources before returning.
        unsafe {
            let screen_dc = GetDC(None);
            if screen_dc.is_invalid() {
                return Err("GetDC(NULL) failed".to_owned());
            }
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            if mem_dc.is_invalid() {
                ReleaseDC(None, screen_dc);
                return Err("CreateCompatibleDC failed".to_owned());
            }
            let bmp = CreateCompatibleBitmap(screen_dc, width, height);
            if bmp.is_invalid() {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err("CreateCompatibleBitmap failed".to_owned());
            }
            let old = SelectObject(mem_dc, bmp.into());
            let printed = PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT).as_bool();

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down DIB
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
            let lines = GetDIBits(
                mem_dc,
                bmp,
                0,
                height as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            SelectObject(mem_dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);

            if !printed || lines == 0 {
                return Err(format!(
                    "PrintWindow/GetDIBits failed (printed={printed}, lines={lines})"
                ));
            }

            // Convert BGRA -> RGBA with opaque alpha.
            for px in bgra.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
                px[3] = 255;
            }
            Ok((width as u32, height as u32, bgra))
        }
    }
}

/// Production [`crate::webview::backend::ClipboardAccess`] backed by native platform clipboard helpers.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformClipboard;

impl crate::webview::backend::ClipboardAccess for PlatformClipboard {
    fn read_text(&self) -> Option<String> {
        read_clipboard_text()
    }

    fn write_text(&self, text: &str) -> bool {
        write_clipboard_text(text)
    }
}
