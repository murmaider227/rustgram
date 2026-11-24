//! System information, theming, DPI, power/battery state, idle time, and shell utilities.
//!
//! Port of `Telegram/lib_base/base/platform/win/base_info_win.cpp`,
//! `base_platform_info.cpp`, `base_last_input_win.cpp`, `base_battery_saving_win.cpp`,
//! `base_power_save_blocker_win.cpp`, `base_file_utilities_win.cpp`,
//! `Telegram/SourceFiles/platform/win/specific_win.cpp`, `file_utilities_win.cpp`,
//! `tray_win.cpp` (`IsDarkTaskbar`), and `main_window_win.cpp` (dark title bar).

use std::path::Path;
use std::time::Duration;

use crate::error::Result;
use crate::registration::RegistryRoot;

/// Straight RGBA color value (`0..=255` per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RgbaColor {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel.
    pub a: u8,
}

impl RgbaColor {
    /// Construct an opaque RGB color.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Construct an RGBA color.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// Numeric Windows OS version (`major.minor.build`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WindowsVersion {
    /// Major version (e.g. `10` for Windows 10 and 11).
    pub major: u32,
    /// Minor version (e.g. `0`).
    pub minor: u32,
    /// OS build number (e.g. `22631` for Windows 11 23H2).
    pub build: u32,
}

impl WindowsVersion {
    /// Returns `true` for Windows 11 or greater (`major > 10 || (major == 10 && build >= 22000)`).
    pub fn is_windows_11_or_greater(self) -> bool {
        self.major > 10 || (self.major == 10 && self.build >= 22000)
    }

    /// Returns `true` for Windows 10 or greater.
    pub fn is_windows_10_or_greater(self) -> bool {
        self.major >= 10
    }

    /// Returns `true` for Windows 8.1 or greater.
    pub fn is_windows_8_1_or_greater(self) -> bool {
        self.major > 6 || (self.major == 6 && self.minor >= 3)
    }

    /// Returns `true` for Windows 8 or greater.
    pub fn is_windows_8_or_greater(self) -> bool {
        self.major > 6 || (self.major == 6 && self.minor >= 2)
    }
}

/// Per-monitor geometry and effective DPI information.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorDpi {
    /// Win32 GDI device name (e.g. `\\.\DISPLAY1`).
    pub device_name: String,
    /// Monitor bounds in virtual screen coordinates.
    pub left: i32,
    /// Monitor top coordinate.
    pub top: i32,
    /// Monitor right coordinate.
    pub right: i32,
    /// Monitor bottom coordinate.
    pub bottom: i32,
    /// Work area bounds excluding taskbar.
    pub work_left: i32,
    /// Work area top coordinate.
    pub work_top: i32,
    /// Work area right coordinate.
    pub work_right: i32,
    /// Work area bottom coordinate.
    pub work_bottom: i32,
    /// Horizontal effective DPI (`96` = 100% scale).
    pub dpi_x: u32,
    /// Vertical effective DPI (`96` = 100% scale).
    pub dpi_y: u32,
    /// Scale factor relative to 96 DPI (`dpi_x as f32 / 96.0`).
    pub scale_factor: f32,
    /// Whether this monitor is the primary display.
    pub is_primary: bool,
}

/// Snapshot of system battery and power-saving state (`base_battery_saving_win.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerStatus {
    /// `true` when running on battery (`ACLineStatus == 0`).
    pub on_battery: bool,
    /// `true` when a system battery is present (`(BatteryFlag & 128) == 0`).
    pub has_battery: bool,
    /// Battery charge percentage `0..=100`, or `None` if unknown (`255`).
    pub battery_percent: Option<u8>,
    /// Windows Battery Saver mode (`SystemStatusFlag == 1`), or `None` if no battery.
    pub battery_saver_enabled: Option<bool>,
}

/// Power-saving blocker kind (`base_power_save_blocker_win.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSaveBlockType {
    /// Prevent system suspension / execution throttling (`PowerRequestExecutionRequired`).
    PreventAppSuspension,
    /// Prevent display sleep (`PowerRequestDisplayRequired`).
    PreventDisplaySleep,
}

/// RAII handle that prevents Windows from sleeping or dimming the display until dropped.
pub struct PowerSaveBlocker {
    #[cfg(windows)]
    _inner: Option<win_sys::PowerRequestGuard>,
}

impl std::fmt::Debug for PowerSaveBlocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PowerSaveBlocker").finish()
    }
}

impl PowerSaveBlocker {
    /// Acquire a system power request with a human-readable `description`.
    pub fn acquire(kind: PowerSaveBlockType, description: &str) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                _inner: Some(win_sys::PowerRequestGuard::acquire(kind, description)?),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (kind, description);
            Ok(Self {})
        }
    }
}

/// RAII watcher for Windows Battery Saver transitions (`GUID_POWER_SAVING_STATUS`).
///
/// Mirrors `BatterySaving` in `base_battery_saving_win.cpp`, using a message-only
/// `HWND_MESSAGE` window on a background thread.
pub struct BatterySavingWatcher {
    #[cfg(windows)]
    _inner: Option<win_sys::BatteryWatcherGuard>,
}

impl std::fmt::Debug for BatterySavingWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatterySavingWatcher").finish()
    }
}

impl BatterySavingWatcher {
    /// Start listening for Windows battery-saving status changes.
    pub fn start<F>(on_changed: F) -> Result<Self>
    where
        F: Fn(Option<bool>) + Send + Sync + 'static,
    {
        #[cfg(windows)]
        {
            Ok(Self {
                _inner: Some(win_sys::BatteryWatcherGuard::start(std::sync::Arc::new(
                    on_changed,
                ))?),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = on_changed;
            Ok(Self {})
        }
    }
}

/// Format the MTProto `initConnection.system_version` string from a Windows version and architecture.
///
/// Mirrors `Platform::SystemVersionPretty()` in `base_info_win.cpp`.
pub fn format_windows_version_pretty(version: WindowsVersion, arch: &str) -> String {
    let os = if version.is_windows_11_or_greater() {
        "Windows 11"
    } else if version.is_windows_10_or_greater() {
        "Windows 10"
    } else if version.is_windows_8_1_or_greater() {
        "Windows 8.1"
    } else if version.is_windows_8_or_greater() {
        "Windows 8"
    } else if version.major == 6 && version.minor == 1 {
        "Windows 7"
    } else {
        "Windows"
    };
    if arch.is_empty() {
        os.to_string()
    } else {
        format!("{os} {arch}")
    }
}

/// Clean, simplify, and pick the best device model string from BIOS registry fields.
///
/// Exact port of `DeviceModelPretty()` in `base_info_win.cpp` and
/// `base_platform_info.cpp` (`SimplifyDeviceModel`, `ProductNameToDeviceModel`,
/// `SimplifyGoodDeviceModel`, `IsDeviceModelOk`, `FinalizeDeviceModel`).
pub fn compute_device_model(
    system_product_name: &str,
    system_family: &str,
    base_board_product: &str,
) -> String {
    const MAX_DEVICE_MODEL_LEN: usize = 15;
    const MAX_GOOD_DEVICE_MODEL_LEN: usize = 32;

    fn simplify(s: &str) -> String {
        let no_underscores = s.replace('_', "");
        no_underscores
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn is_ok(s: &str) -> bool {
        !s.is_empty() && s.chars().count() <= MAX_DEVICE_MODEL_LEN
    }

    fn simplify_good(model: &str, remove: &[&str]) -> String {
        let mut result = String::new();
        for word in model.split(' ') {
            let lower = word.to_ascii_lowercase();
            if remove.contains(&lower.as_str()) {
                continue;
            }
            if result.is_empty() {
                result.push_str(word);
            } else if result.chars().count() + word.chars().count() + 1 > MAX_GOOD_DEVICE_MODEL_LEN
            {
                return result;
            } else {
                result.push(' ');
                result.push_str(word);
            }
        }
        result
    }

    let product = simplify(system_product_name);
    let candidate = if product.starts_with("HP ") {
        simplify_good(
            &product,
            &["notebook", "desktop", "mobile", "workstation", "pc"],
        )
    } else if is_ok(&product) {
        product
    } else {
        let family = simplify(system_family);
        let board = simplify(base_board_product);
        let combined = simplify(&format!("{family} {board}"));
        if is_ok(&combined) {
            combined
        } else if is_ok(&board) {
            board
        } else if is_ok(&family) {
            family
        } else {
            String::new()
        }
    };

    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        "Desktop".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Sanitize a user-supplied filename for safe saving on Windows.
///
/// Exact port of `FileNameFromUserString` in `base_file_utilities_win.cpp`:
/// masks `.lnk`/`.scf` with `.download` and prefixes reserved DOS device names
/// (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`) with `_`.
pub fn sanitize_filename(name: &str) -> String {
    let mut out = name.to_string();
    let lower = out.to_ascii_lowercase();
    if lower.ends_with(".lnk") || lower.ends_with(".scf") {
        out.push_str(".download");
    }

    const BAD_NAMES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let upper = out.to_ascii_uppercase();
    for bad in BAD_NAMES {
        if upper.starts_with(bad) {
            let next = out.as_bytes().get(bad.len()).copied();
            if next.is_none() || next == Some(b'.') {
                out.insert(0, '_');
                break;
            }
        }
    }
    out
}

/// Query the real Windows OS version (`RtlGetVersion` on Windows).
pub fn windows_version() -> WindowsVersion {
    #[cfg(windows)]
    {
        win_sys::windows_version()
    }
    #[cfg(not(windows))]
    {
        WindowsVersion {
            major: 0,
            minor: 0,
            build: 0,
        }
    }
}

/// Return the formatted Windows version string suitable for MTProto `initConnection.system_version`
/// (e.g. `"Windows 11 x64"`).
pub fn system_version_pretty() -> String {
    #[cfg(windows)]
    {
        win_sys::system_version_pretty()
    }
    #[cfg(not(windows))]
    {
        std::env::consts::OS.to_string()
    }
}

/// Return the simplified hardware device model suitable for MTProto `initConnection.device_model`.
pub fn device_model_pretty() -> String {
    #[cfg(windows)]
    {
        win_sys::device_model_pretty()
    }
    #[cfg(not(windows))]
    {
        "Desktop".to_string()
    }
}

/// Return the two-letter ISO 3166 country code for the current user (`base_info_win.cpp`).
pub fn system_country() -> String {
    #[cfg(windows)]
    {
        win_sys::system_country()
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

/// Return the BCP-47 UI language tag for the current user (e.g. `"en-US"`).
pub fn system_language() -> String {
    #[cfg(windows)]
    {
        win_sys::system_language()
    }
    #[cfg(not(windows))]
    {
        "en".to_string()
    }
}

/// Return how long has elapsed since the last keyboard or mouse input (`GetLastInputInfo`).
pub fn idle_time() -> Option<Duration> {
    #[cfg(windows)]
    {
        win_sys::idle_time()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Check whether Windows dark mode for apps is enabled (`AppsUseLightTheme == 0`).
///
/// Returns `None` on pre-1809 Windows, when High Contrast mode is active, or if unavailable.
pub fn is_dark_mode() -> Option<bool> {
    is_dark_mode_in(&RegistryRoot::hkcu())
}

/// Parameterized variant of [`is_dark_mode`] reading from `root` for unit testing.
pub fn is_dark_mode_in(root: &RegistryRoot) -> Option<bool> {
    #[cfg(windows)]
    {
        win_sys::is_dark_mode_in(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        None
    }
}

/// Check whether the Windows taskbar uses dark mode (`SystemUsesLightTheme == 0`).
pub fn is_dark_taskbar() -> Option<bool> {
    is_dark_taskbar_in(&RegistryRoot::hkcu())
}

/// Parameterized variant of [`is_dark_taskbar`] reading from `root` for unit testing.
pub fn is_dark_taskbar_in(root: &RegistryRoot) -> Option<bool> {
    #[cfg(windows)]
    {
        win_sys::is_dark_taskbar_in(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        None
    }
}

/// Enable or disable the Windows 10/11 dark window title bar (`DWMWA_USE_IMMERSIVE_DARK_MODE`).
pub fn set_window_dark_mode(hwnd: isize, dark: bool) -> Result<()> {
    #[cfg(windows)]
    {
        win_sys::set_window_dark_mode(hwnd, dark)
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, dark);
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Query the user's Windows accent color (from DWM `AccentColor` registry value or `DwmGetColorizationColor`).
pub fn system_accent_color() -> Option<RgbaColor> {
    system_accent_color_in(&RegistryRoot::hkcu())
}

/// Parameterized variant of [`system_accent_color`] reading from `root` for unit testing.
pub fn system_accent_color_in(root: &RegistryRoot) -> Option<RgbaColor> {
    #[cfg(windows)]
    {
        win_sys::system_accent_color_in(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        None
    }
}

/// Enumerate all active monitors with their effective DPI and work area.
pub fn monitors_dpi() -> Vec<MonitorDpi> {
    #[cfg(windows)]
    {
        win_sys::monitors_dpi()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Return the effective DPI for a specific window handle (`GetDpiForWindow`).
pub fn dpi_for_window(hwnd: isize) -> u32 {
    #[cfg(windows)]
    {
        win_sys::dpi_for_window(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        96
    }
}

/// Return the system-wide DPI (`GetDpiForSystem`, typically `96` at 100% scaling).
pub fn system_dpi() -> u32 {
    #[cfg(windows)]
    {
        win_sys::system_dpi()
    }
    #[cfg(not(windows))]
    {
        96
    }
}

/// Query the current system power and battery-saver status (`GetSystemPowerStatus`).
pub fn power_status() -> Option<PowerStatus> {
    #[cfg(windows)]
    {
        win_sys::power_status()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Open Windows Explorer and select `path` (`ShowInFolder` in `base_file_utilities_win.cpp`).
pub fn show_in_folder(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        win_sys::show_in_folder(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Open a URL with the default registered browser/handler (`ShellExecuteW("open", url)`).
pub fn open_url(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        win_sys::shell_open(url)
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Open a local file or folder with its default shell handler (`UnsafeLaunch`).
pub fn open_file(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        win_sys::shell_open(&path.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Open a `mailto:` link using the system default mail client (`UnsafeOpenEmailLink`).
pub fn open_mailto(email: &str) -> Result<()> {
    let uri = if email.to_ascii_lowercase().starts_with("mailto:") {
        email.to_string()
    } else {
        format!("mailto:{email}")
    };
    open_url(&uri)
}

/// Return the ProgID / application name of the user's default `mailto:` handler, if configured.
pub fn default_mail_app() -> Option<String> {
    #[cfg(windows)]
    {
        win_sys::default_mail_app()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Mark a downloaded file with the NTFS Internet security zone alternate data stream
/// (`:Zone.Identifier`, `ZoneId=3`), respecting the `SaveZoneInformation` policy
/// (`PostprocessDownloaded` in `file_utilities_win.cpp`).
pub fn mark_downloaded_zone_identifier(path: &Path) -> Result<()> {
    mark_downloaded_zone_identifier_with_policy(path, &RegistryRoot::hkcu())
}

/// Parameterized variant of [`mark_downloaded_zone_identifier`] for testing policy behavior.
pub fn mark_downloaded_zone_identifier_with_policy(
    path: &Path,
    policy_root: &RegistryRoot,
) -> Result<()> {
    #[cfg(windows)]
    {
        win_sys::mark_downloaded_zone_identifier(path, policy_root)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, policy_root);
        Ok(())
    }
}

/// Enable or disable screenshot/capture exclusion on a window
/// (`SetWindowScreenshotProtection` in `specific_win.cpp`).
pub fn set_window_screenshot_protection(hwnd: isize, enabled: bool) -> Result<()> {
    #[cfg(windows)]
    {
        win_sys::set_window_screenshot_protection(hwnd, enabled)
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, enabled);
        Err(crate::error::PlatformError::Unsupported)
    }
}

#[cfg(windows)]
mod win_sys {
    use super::*;
    use crate::app_id::win_impl::{ComGuard, to_wide};
    use crate::error::PlatformError;
    use crate::registration::win_reg::{open_reg_key, read_reg_dword, read_reg_sz};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{
        COLORREF, CloseHandle, HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT,
        WPARAM,
    };
    use windows::Win32::Globalization::{
        GetLocaleInfoEx, GetUserDefaultUILanguage, LOCALE_SISO3166CTRYNAME, LOCALE_SNAME,
    };
    use windows::Win32::Graphics::Dwm::{
        DWMWA_USE_IMMERSIVE_DARK_MODE, DwmGetColorizationColor, DwmSetWindowAttribute,
    };
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Power::{
        GetSystemPowerStatus, HPOWERNOTIFY, PowerClearRequest, PowerCreateRequest,
        PowerRequestDisplayRequired, PowerRequestExecutionRequired, PowerSetRequest,
        RegisterPowerSettingNotification, SYSTEM_POWER_STATUS, UnregisterPowerSettingNotification,
    };
    use windows::Win32::System::SystemInformation::{
        GetNativeSystemInfo, GetTickCount, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64,
        IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386, OSVERSIONINFOW,
        PROCESSOR_ARCHITECTURE_AMD64, PROCESSOR_ARCHITECTURE_ARM64, PROCESSOR_ARCHITECTURE_INTEL,
        SYSTEM_INFO,
    };
    use windows::Win32::System::SystemServices::{
        GUID_POWER_SAVING_STATUS, POWER_REQUEST_CONTEXT_VERSION,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, IsWow64Process2, POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT,
        REASON_CONTEXT_0,
    };
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::HiDpi::{
        GetDpiForMonitor, GetDpiForSystem, GetDpiForWindow, MDT_EFFECTIVE_DPI,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::Shell::{
        AL_EFFECTIVE, AT_URLPROTOCOL, ApplicationAssociationRegistration,
        IApplicationAssociationRegistration, ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems,
        ShellExecuteW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CREATESTRUCTW, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow,
        DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, MSG,
        PostMessageW, PostQuitMessage, RegisterClassExW, SPI_GETHIGHCONTRAST, SW_SHOWNORMAL,
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowDisplayAffinity, SetWindowLongPtrW,
        SystemParametersInfoW, TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_NCCREATE, WM_POWERBROADCAST,
        WNDCLASSEXW,
    };
    use windows::core::{BOOL, PCSTR, PCWSTR, PWSTR};

    const PERSONALIZE_KEY: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
    const DWM_KEY: &str = "Software\\Microsoft\\Windows\\DWM";

    pub(crate) fn windows_version() -> WindowsVersion {
        type RtlGetVersionFn = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        let ntdll_w = to_wide("ntdll.dll");
        // SAFETY: Querying `RtlGetVersion` from ntdll.dll (always loaded) to obtain the true
        // OS build number independent of application manifest shims.
        unsafe {
            if let Ok(ntdll) = LoadLibraryW(PCWSTR(ntdll_w.as_ptr()))
                && let Some(proc) = GetProcAddress(ntdll, PCSTR(c"RtlGetVersion".as_ptr().cast()))
            {
                let f: RtlGetVersionFn = std::mem::transmute(proc);
                if f(&mut info) == 0 {
                    return WindowsVersion {
                        major: info.dwMajorVersion,
                        minor: info.dwMinorVersion,
                        build: info.dwBuildNumber,
                    };
                }
            }
        }
        WindowsVersion {
            major: 10,
            minor: 0,
            build: 0,
        }
    }

    pub(crate) fn system_version_pretty() -> String {
        let ver = windows_version();
        let mut process_machine = IMAGE_FILE_MACHINE::default();
        let mut native_machine = IMAGE_FILE_MACHINE::default();

        // SAFETY: Querying native machine architecture for the current process.
        let arch = if unsafe {
            IsWow64Process2(
                GetCurrentProcess(),
                &mut process_machine,
                Some(&mut native_machine),
            )
        }
        .is_ok()
        {
            match native_machine {
                IMAGE_FILE_MACHINE_AMD64 => "x64",
                IMAGE_FILE_MACHINE_ARM64 => "arm64",
                IMAGE_FILE_MACHINE_I386 => "x86",
                _ => "",
            }
        } else {
            let mut si = SYSTEM_INFO::default();
            // SAFETY: GetNativeSystemInfo writes to an initialized SYSTEM_INFO struct.
            unsafe { GetNativeSystemInfo(&mut si) };
            // SAFETY: Reading processor architecture union field.
            let pa = unsafe { si.Anonymous.Anonymous.wProcessorArchitecture };
            if pa == PROCESSOR_ARCHITECTURE_AMD64 {
                "x64"
            } else if pa == PROCESSOR_ARCHITECTURE_ARM64 {
                "arm64"
            } else if pa == PROCESSOR_ARCHITECTURE_INTEL {
                "x86"
            } else {
                ""
            }
        };

        format_windows_version_pretty(ver, arch)
    }

    pub(crate) fn device_model_pretty() -> String {
        let root = RegistryRoot::hklm();
        let key = open_reg_key(&root, "HARDWARE\\DESCRIPTION\\System\\BIOS", false)
            .ok()
            .flatten();
        let (product, family, board) = match key {
            Some(k) => (
                read_reg_sz(k.0, Some("SystemProductName")).unwrap_or_default(),
                read_reg_sz(k.0, Some("SystemFamily")).unwrap_or_default(),
                read_reg_sz(k.0, Some("BaseBoardProduct")).unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        compute_device_model(&product, &family, &board)
    }

    pub(crate) fn system_country() -> String {
        if let Ok(Some(k)) = open_reg_key(
            &RegistryRoot::hkcu(),
            "Control Panel\\International\\Geo",
            false,
        ) && let Some(name) = read_reg_sz(k.0, Some("Name"))
            && name.len() == 2
        {
            return name;
        }
        let mut buf = [0u16; 64];
        // SAFETY: Valid output buffer for GetLocaleInfoEx (NULL = LOCALE_NAME_USER_DEFAULT).
        let len =
            unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_SISO3166CTRYNAME, Some(&mut buf)) };
        if len > 1 {
            String::from_utf16_lossy(&buf[..(len as usize - 1)])
        } else {
            String::new()
        }
    }

    pub(crate) fn system_language() -> String {
        let mut buf = [0u16; 128];
        // SAFETY: Valid output buffer for GetLocaleInfoEx (NULL = LOCALE_NAME_USER_DEFAULT).
        let len = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_SNAME, Some(&mut buf)) };
        if len > 1 {
            return String::from_utf16_lossy(&buf[..(len as usize - 1)]);
        }
        // SAFETY: Pure query of current user's UI language ID.
        let lang_id = unsafe { GetUserDefaultUILanguage() };
        format!("{lang_id:#06x}")
    }

    pub(crate) fn idle_time() -> Option<Duration> {
        let mut lii = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `lii.cbSize` is initialized before calling GetLastInputInfo.
        if !unsafe { GetLastInputInfo(&mut lii) }.as_bool() {
            return None;
        }
        // SAFETY: Pure tick counter query; `LASTINPUTINFO.dwTime` uses 32-bit `GetTickCount`,
        // so wrapping subtraction handles the 49.7-day rollover cleanly.
        let ticks32 = unsafe { GetTickCount() };
        let elapsed_ms = u64::from(ticks32.wrapping_sub(lii.dwTime));
        Some(Duration::from_millis(elapsed_ms))
    }

    fn is_high_contrast_enabled() -> bool {
        let mut hc = HIGHCONTRASTW {
            cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        // SAFETY: `hc.cbSize` is set; SystemParametersInfoW fills `hc`.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                hc.cbSize,
                Some((&mut hc as *mut HIGHCONTRASTW).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .is_ok();
        ok && (hc.dwFlags.0 & HCF_HIGHCONTRASTON.0) != 0
    }

    pub(crate) fn is_dark_mode_in(root: &RegistryRoot) -> Option<bool> {
        if root.prefix.is_empty() {
            let ver = windows_version();
            if !(ver.major > 10 || (ver.major == 10 && ver.build >= 17763)) {
                return None;
            }
            if is_high_contrast_enabled() {
                return None;
            }
        }
        read_reg_dword(root, PERSONALIZE_KEY, "AppsUseLightTheme").map(|v| v == 0)
    }

    pub(crate) fn is_dark_taskbar_in(root: &RegistryRoot) -> Option<bool> {
        if root.prefix.is_empty() {
            let ver = windows_version();
            if !(ver.major > 10 || (ver.major == 10 && ver.build >= 18282)) {
                return None;
            }
        }
        read_reg_dword(root, PERSONALIZE_KEY, "SystemUsesLightTheme").map(|v| v == 0)
    }

    pub(crate) fn set_window_dark_mode(hwnd: isize, dark: bool) -> Result<()> {
        let ver = windows_version();
        if !(ver.major > 10 || (ver.major == 10 && ver.build >= 17763)) {
            return Err(PlatformError::Unsupported);
        }
        let attr = if ver.major > 10 || ver.build >= 18985 {
            DWMWA_USE_IMMERSIVE_DARK_MODE
        } else {
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(19)
        };
        let value = BOOL::from(dark);
        // SAFETY: Passing a valid 4-byte BOOL to DwmSetWindowAttribute for `hwnd`.
        unsafe {
            DwmSetWindowAttribute(
                HWND(hwnd as *mut _),
                attr,
                (&value as *const BOOL).cast(),
                std::mem::size_of::<BOOL>() as u32,
            )
        }
        .map_err(|e| PlatformError::from_win("DwmSetWindowAttribute", e))
    }

    pub(crate) fn system_accent_color_in(root: &RegistryRoot) -> Option<RgbaColor> {
        // Windows 10/11 stores AccentColor in HKCU\Software\Microsoft\Windows\DWM as ABGR DWORD.
        if let Some(abgr) = read_reg_dword(root, DWM_KEY, "AccentColor") {
            let r = (abgr & 0xFF) as u8;
            let g = ((abgr >> 8) & 0xFF) as u8;
            let b = ((abgr >> 16) & 0xFF) as u8;
            let a = ((abgr >> 24) & 0xFF) as u8;
            return Some(RgbaColor::rgba(r, g, b, if a == 0 { 255 } else { a }));
        }
        if !root.prefix.is_empty() {
            return None;
        }
        let mut color: u32 = 0;
        let mut opaque = BOOL(0);
        // SAFETY: DwmGetColorizationColor returns AARRGGBB in `color`.
        if unsafe { DwmGetColorizationColor(&mut color, &mut opaque) }.is_ok() {
            let a = ((color >> 24) & 0xFF) as u8;
            let r = ((color >> 16) & 0xFF) as u8;
            let g = ((color >> 8) & 0xFF) as u8;
            let b = (color & 0xFF) as u8;
            return Some(RgbaColor::rgba(r, g, b, if a == 0 { 255 } else { a }));
        }
        None
    }

    unsafe extern "system" fn enum_monitor_cb(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` points to a `Vec<MonitorDpi>` alive for the duration of EnumDisplayMonitors.
        let out = unsafe { &mut *(lparam.0 as *mut Vec<MonitorDpi>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: Filling MONITORINFOEXW for `hmonitor`.
        if !unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo) }.as_bool() {
            return BOOL(1);
        }
        let mut dpi_x: u32 = 96;
        let mut dpi_y: u32 = 96;
        // SAFETY: Querying per-monitor effective DPI.
        let _ = unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        let end = info
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szDevice.len());
        let device_name = OsString::from_wide(&info.szDevice[..end])
            .to_string_lossy()
            .into_owned();
        let rc = info.monitorInfo.rcMonitor;
        let work = info.monitorInfo.rcWork;
        let is_primary = (info.monitorInfo.dwFlags & 1) != 0; // MONITORINFOF_PRIMARY

        out.push(MonitorDpi {
            device_name,
            left: rc.left,
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
            work_left: work.left,
            work_top: work.top,
            work_right: work.right,
            work_bottom: work.bottom,
            dpi_x,
            dpi_y,
            scale_factor: dpi_x as f32 / 96.0,
            is_primary,
        });
        BOOL(1)
    }

    pub(crate) fn monitors_dpi() -> Vec<MonitorDpi> {
        let mut result = Vec::new();
        // SAFETY: EnumDisplayMonitors invokes `enum_monitor_cb` synchronously.
        let _ = unsafe {
            EnumDisplayMonitors(
                None,
                None,
                Some(enum_monitor_cb),
                LPARAM(&mut result as *mut Vec<MonitorDpi> as isize),
            )
        };
        result
    }

    pub(crate) fn dpi_for_window(hwnd: isize) -> u32 {
        // SAFETY: GetDpiForWindow returns 0 on invalid HWND, in which case we fall back to system_dpi.
        let dpi = unsafe { GetDpiForWindow(HWND(hwnd as *mut _)) };
        if dpi == 0 { system_dpi() } else { dpi }
    }

    pub(crate) fn system_dpi() -> u32 {
        // SAFETY: Pure system DPI query.
        let dpi = unsafe { GetDpiForSystem() };
        if dpi == 0 { 96 } else { dpi }
    }

    pub(crate) fn power_status() -> Option<PowerStatus> {
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: Filling SYSTEM_POWER_STATUS via Win32 API.
        unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
        let has_battery = (status.BatteryFlag & 128) == 0;
        let on_battery = status.ACLineStatus == 0;
        let battery_percent = if status.BatteryLifePercent <= 100 {
            Some(status.BatteryLifePercent)
        } else {
            None
        };
        let battery_saver_enabled = if has_battery {
            Some(status.SystemStatusFlag == 1)
        } else {
            None
        };
        Some(PowerStatus {
            on_battery,
            has_battery,
            battery_percent,
            battery_saver_enabled,
        })
    }

    pub(crate) struct PowerRequestGuard {
        handle: HANDLE,
        kind: PowerSaveBlockType,
    }

    // SAFETY: Power request handle is exclusively owned and released in Drop.
    unsafe impl Send for PowerRequestGuard {}
    unsafe impl Sync for PowerRequestGuard {}

    impl PowerRequestGuard {
        pub(crate) fn acquire(kind: PowerSaveBlockType, description: &str) -> Result<Self> {
            let mut desc_w = to_wide(description);
            let ctx = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    SimpleReasonString: PWSTR(desc_w.as_mut_ptr()),
                },
            };
            // SAFETY: `desc_w` stays alive during PowerCreateRequest.
            let handle = unsafe { PowerCreateRequest(&ctx) }
                .map_err(|e| PlatformError::from_win("PowerCreateRequest", e))?;
            if handle.is_invalid() || handle == INVALID_HANDLE_VALUE {
                return Err(PlatformError::last_win32("PowerCreateRequest"));
            }
            let req_type = match kind {
                PowerSaveBlockType::PreventDisplaySleep => PowerRequestDisplayRequired,
                PowerSaveBlockType::PreventAppSuspension => PowerRequestExecutionRequired,
            };
            // SAFETY: `handle` is valid; if PowerSetRequest fails, close handle before returning.
            if let Err(e) = unsafe { PowerSetRequest(handle, req_type) } {
                let _ = unsafe { CloseHandle(handle) };
                return Err(PlatformError::from_win("PowerSetRequest", e));
            }
            Ok(Self { handle, kind })
        }
    }

    impl Drop for PowerRequestGuard {
        fn drop(&mut self) {
            if !self.handle.is_invalid() {
                let req_type = match self.kind {
                    PowerSaveBlockType::PreventDisplaySleep => PowerRequestDisplayRequired,
                    PowerSaveBlockType::PreventAppSuspension => PowerRequestExecutionRequired,
                };
                // SAFETY: Paired with PowerSetRequest / PowerCreateRequest.
                unsafe {
                    let _ = PowerClearRequest(self.handle, req_type);
                    let _ = CloseHandle(self.handle);
                }
            }
        }
    }

    pub(crate) struct BatteryWatcherGuard {
        hwnd_raw: isize,
        thread: Option<JoinHandle<()>>,
    }

    impl BatteryWatcherGuard {
        pub(crate) fn start(cb: Arc<dyn Fn(Option<bool>) + Send + Sync>) -> Result<Self> {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<isize>>(1);
            let thread = std::thread::Builder::new()
                .name("tg-battery-watcher".into())
                .spawn(move || {
                    let class_name = to_wide("RustgramBatterySavingWatcher");
                    // SAFETY: Retrieving module handle for the current executable.
                    let hinst: HMODULE = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
                    let wc = WNDCLASSEXW {
                        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                        lpfnWndProc: Some(battery_wnd_proc),
                        hInstance: hinst.into(),
                        lpszClassName: PCWSTR(class_name.as_ptr()),
                        ..Default::default()
                    };
                    // SAFETY: Registering window class (already exists is harmless).
                    unsafe { RegisterClassExW(&wc) };

                    let boxed_cb = Box::new(cb);
                    let cb_ptr = Box::into_raw(boxed_cb);

                    // SAFETY: Creating an invisible message-only window (`HWND_MESSAGE`).
                    let hwnd = match unsafe {
                        CreateWindowExW(
                            WINDOW_EX_STYLE(0),
                            PCWSTR(class_name.as_ptr()),
                            PCWSTR::null(),
                            WINDOW_STYLE(0),
                            0,
                            0,
                            0,
                            0,
                            Some(HWND_MESSAGE),
                            None,
                            Some(hinst.into()),
                            Some(cb_ptr.cast()),
                        )
                    } {
                        Ok(h) if !h.0.is_null() => h,
                        Err(e) => {
                            // SAFETY: Reclaiming leaked Box on window creation failure.
                            let _ = unsafe { Box::from_raw(cb_ptr) };
                            let _ = tx.send(Err(PlatformError::from_win("CreateWindowExW", e)));
                            return;
                        }
                        Ok(_) => {
                            let _ = unsafe { Box::from_raw(cb_ptr) };
                            let _ = tx.send(Err(PlatformError::last_win32("CreateWindowExW")));
                            return;
                        }
                    };

                    // SAFETY: Registering power setting notifications on our message-only window.
                    let notify: HPOWERNOTIFY = unsafe {
                        RegisterPowerSettingNotification(
                            HANDLE(hwnd.0),
                            &GUID_POWER_SAVING_STATUS,
                            DEVICE_NOTIFY_WINDOW_HANDLE,
                        )
                    }
                    .unwrap_or_default();

                    let _ = tx.send(Ok(hwnd.0 as isize));

                    let mut msg = MSG::default();
                    // SAFETY: Standard Win32 message loop for this thread's message-only window.
                    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                        unsafe {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }

                    if !notify.is_invalid() {
                        // SAFETY: Unregistering power setting notification before exiting.
                        let _ = unsafe { UnregisterPowerSettingNotification(notify) };
                    }
                    // SAFETY: Reclaiming callback Box after window is destroyed.
                    let _ = unsafe { Box::from_raw(cb_ptr) };
                })?;

            let hwnd_raw = rx.recv().map_err(|_| PlatformError::Win32 {
                context: "BatterySavingWatcher",
                code: 0,
                message: "watcher thread exited prematurely".into(),
            })??;
            Ok(Self {
                hwnd_raw,
                thread: Some(thread),
            })
        }
    }

    impl Drop for BatteryWatcherGuard {
        fn drop(&mut self) {
            if self.hwnd_raw != 0 {
                // SAFETY: Posting WM_CLOSE to our message-only window to exit its loop cleanly.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(self.hwnd_raw as *mut _)),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    unsafe extern "system" fn battery_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_NCCREATE => {
                // SAFETY: `lparam` points to CREATESTRUCTW during WM_NCCREATE.
                let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_POWERBROADCAST => {
                // SAFETY: Reading callback pointer stored in GWLP_USERDATA.
                let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) }
                    as *const Arc<dyn Fn(Option<bool>) + Send + Sync>;
                if !ptr.is_null() {
                    let cb = unsafe { &*ptr };
                    let state = power_status().and_then(|s| s.battery_saver_enabled);
                    cb(state);
                }
                LRESULT(1)
            }
            WM_CLOSE => {
                // SAFETY: Destroying our owned message-only window.
                let _ = unsafe { DestroyWindow(hwnd) };
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: Ending the watcher thread's message loop.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    pub(crate) fn show_in_folder(path: &Path) -> Result<()> {
        let _com = ComGuard::init_sta();
        let native = path.to_string_lossy().replace('/', "\\");
        let wide = to_wide(&native);
        // SAFETY: ILCreateFromPathW allocates an ITEMIDLIST freed via ILFree.
        let pidl = unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) };
        if !pidl.is_null() {
            // SAFETY: Valid PIDL; selects the item in Explorer.
            let res = unsafe { SHOpenFolderAndSelectItems(pidl, None, 0) };
            // SAFETY: Freeing PIDL allocated by ILCreateFromPathW.
            unsafe { ILFree(Some(pidl)) };
            if res.is_ok() {
                return Ok(());
            }
        }
        // Fallback matching `base_file_utilities_win.cpp`.
        let escaped = native.replace('"', "\"\"");
        let params = format!("/select,{escaped}");
        let exe_w = to_wide("explorer");
        let params_w = to_wide(&params);
        // SAFETY: Launching explorer.exe with `/select,<path>`.
        let hinst = unsafe {
            ShellExecuteW(
                None,
                PCWSTR::null(),
                PCWSTR(exe_w.as_ptr()),
                PCWSTR(params_w.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if (hinst.0 as isize) > 32 {
            Ok(())
        } else {
            Err(PlatformError::Win32 {
                context: "ShellExecuteW(explorer)",
                code: hinst.0 as usize as u32,
                message: "failed to open folder in Explorer".into(),
            })
        }
    }

    pub(crate) fn shell_open(target: &str) -> Result<()> {
        let verb = to_wide("open");
        let target_w = to_wide(target);
        // SAFETY: Invoking ShellExecuteW("open", target) with null-terminated wide strings.
        let hinst = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(target_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if (hinst.0 as isize) > 32 {
            Ok(())
        } else {
            Err(PlatformError::Win32 {
                context: "ShellExecuteW",
                code: hinst.0 as usize as u32,
                message: format!("failed to open '{target}'"),
            })
        }
    }

    pub(crate) fn default_mail_app() -> Option<String> {
        let _com = ComGuard::init_sta();
        // SAFETY: Standard Shell COM class querying effective URL protocol handler for `mailto`.
        let aar: IApplicationAssociationRegistration = unsafe {
            CoCreateInstance(
                &ApplicationAssociationRegistration,
                None,
                CLSCTX_INPROC_SERVER,
            )
        }
        .ok()?;
        let mailto = to_wide("mailto");
        // SAFETY: Returns CoTaskMem-allocated PWSTR which we copy and free.
        let handler = unsafe {
            aar.QueryCurrentDefault(PCWSTR(mailto.as_ptr()), AT_URLPROTOCOL, AL_EFFECTIVE)
        }
        .ok()?;
        let name = unsafe { handler.to_string() }.ok();
        // SAFETY: Freeing PWSTR allocated by QueryCurrentDefault.
        unsafe { CoTaskMemFree(Some(handler.0 as *const _)) };
        name.filter(|s| !s.is_empty())
    }

    pub(crate) fn mark_downloaded_zone_identifier(
        path: &Path,
        policy_root: &RegistryRoot,
    ) -> Result<()> {
        // Check `SaveZoneInformation` policy (`file_utilities_win.cpp` lines 120-141).
        if let Some(1) = read_reg_dword(
            policy_root,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Policies\\Attachments",
            "SaveZoneInformation",
        ) {
            return Ok(());
        }
        let native = path.to_string_lossy().replace('/', "\\");
        let ads_path = format!("{native}:Zone.Identifier");
        std::fs::write(&ads_path, b"[ZoneTransfer]\r\nZoneId=3\r\n")?;
        Ok(())
    }

    pub(crate) fn set_window_screenshot_protection(hwnd: isize, enabled: bool) -> Result<()> {
        let h = HWND(hwnd as *mut _);
        if !enabled {
            // SAFETY: Clearing display affinity on `h`.
            unsafe { SetWindowDisplayAffinity(h, WDA_NONE) }
                .map_err(|e| PlatformError::from_win("SetWindowDisplayAffinity(WDA_NONE)", e))?;
            return Ok(());
        }
        // SAFETY: Try WDA_EXCLUDEFROMCAPTURE first (Win10 2004+), fall back to WDA_MONITOR.
        if unsafe { SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE) }.is_err() {
            unsafe { SetWindowDisplayAffinity(h, WDA_MONITOR) }
                .map_err(|e| PlatformError::from_win("SetWindowDisplayAffinity(WDA_MONITOR)", e))?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn colorref_from_rgba(c: RgbaColor) -> COLORREF {
        COLORREF(u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_version_formatting() {
        assert_eq!(
            format_windows_version_pretty(
                WindowsVersion {
                    major: 10,
                    minor: 0,
                    build: 22631
                },
                "x64"
            ),
            "Windows 11 x64"
        );
        assert_eq!(
            format_windows_version_pretty(
                WindowsVersion {
                    major: 10,
                    minor: 0,
                    build: 19045
                },
                "x64"
            ),
            "Windows 10 x64"
        );
        assert_eq!(
            format_windows_version_pretty(
                WindowsVersion {
                    major: 10,
                    minor: 0,
                    build: 26100
                },
                "arm64"
            ),
            "Windows 11 arm64"
        );
        assert_eq!(
            format_windows_version_pretty(
                WindowsVersion {
                    major: 6,
                    minor: 3,
                    build: 9600
                },
                "x64"
            ),
            "Windows 8.1 x64"
        );
    }

    #[test]
    fn device_model_simplification_matches_tdesktop() {
        assert_eq!(
            compute_device_model("HP EliteBook 840 G8 Notebook PC", "", ""),
            "HP EliteBook 840 G8"
        );
        assert_eq!(compute_device_model("XPS_15_9520", "", ""), "XPS159520");
        assert_eq!(
            compute_device_model("System Product Name Too Long For Limit", "ROG", "B550-F"),
            "ROG B550-F"
        );
        assert_eq!(
            compute_device_model(
                "Very Long Unusable System Product Name",
                "Very Long System Family Name",
                "Very Long Base Board Product Name"
            ),
            "Desktop"
        );
    }

    #[test]
    fn filename_sanitization_matches_tdesktop() {
        assert_eq!(sanitize_filename("photo.jpg"), "photo.jpg");
        assert_eq!(sanitize_filename("evil.lnk"), "evil.lnk.download");
        assert_eq!(sanitize_filename("evil.SCF"), "evil.SCF.download");
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_filename("com1.log"), "_com1.log");
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
    }

    #[cfg(windows)]
    #[test]
    fn dark_mode_and_accent_color_from_throwaway_registry() {
        use crate::registration::win_reg::write_reg_dword;

        let root = RegistryRoot::hkcu_subkey(format!(
            "Software\\RustgramTest\\sysinfo_{}",
            std::process::id()
        ));
        let _ = root.delete_tree();

        assert_eq!(is_dark_mode_in(&root), None);
        write_reg_dword(
            &root,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            "AppsUseLightTheme",
            0,
        )
        .unwrap();
        assert_eq!(is_dark_mode_in(&root), Some(true));

        write_reg_dword(
            &root,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            "AppsUseLightTheme",
            1,
        )
        .unwrap();
        assert_eq!(is_dark_mode_in(&root), Some(false));

        // ABGR: A=0xFF, B=0xD4, G=0x78, R=0x00 -> RGB(0, 120, 212)
        write_reg_dword(
            &root,
            "Software\\Microsoft\\Windows\\DWM",
            "AccentColor",
            0xFFD4_7800,
        )
        .unwrap();
        assert_eq!(
            system_accent_color_in(&root),
            Some(RgbaColor::rgba(0x00, 0x78, 0xD4, 0xFF))
        );

        root.delete_tree().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn read_only_system_queries_work() {
        let ver = windows_version();
        assert!(ver.major >= 10);
        let pretty = system_version_pretty();
        assert!(pretty.starts_with("Windows "));
        assert!(!device_model_pretty().is_empty());
        assert!(idle_time().is_some());
        assert!(system_dpi() >= 96);
        let monitors = monitors_dpi();
        assert!(!monitors.is_empty());
        assert!(monitors.iter().all(|m| m.dpi_x >= 96 && m.dpi_y >= 96));
    }

    #[cfg(windows)]
    #[test]
    fn zone_identifier_and_power_save_blocker() {
        let tmp =
            std::env::temp_dir().join(format!("rustgram_zone_test_{}.txt", std::process::id()));
        std::fs::write(&tmp, b"hello").unwrap();
        let root = RegistryRoot::hkcu_subkey(format!(
            "Software\\RustgramTest\\zone_{}",
            std::process::id()
        ));
        let _ = root.delete_tree();

        // Default policy preserves zone info.
        mark_downloaded_zone_identifier_with_policy(&tmp, &root).unwrap();
        let ads = std::fs::read_to_string(format!("{}:Zone.Identifier", tmp.display())).unwrap();
        assert!(ads.contains("ZoneId=3"));

        // Acquiring and immediately releasing a power save blocker succeeds.
        let blocker = PowerSaveBlocker::acquire(
            PowerSaveBlockType::PreventAppSuspension,
            "Rustgram unit test",
        )
        .unwrap();
        drop(blocker);

        let _ = std::fs::remove_file(&tmp);
        let _ = root.delete_tree();
    }
}
