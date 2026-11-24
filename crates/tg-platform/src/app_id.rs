//! AppUserModelID and Start-menu shortcut management.
//!
//! Port of `Telegram/SourceFiles/platform/win/windows_app_user_model_id.cpp`.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// Default Windows AppUserModelID used by Rustgram.
pub const DEFAULT_APP_USER_MODEL_ID: &str = "Rustgram";

/// Default Start-menu shortcut file name created for WinRT toast notifications.
pub const DEFAULT_SHORTCUT_NAME: &str = "Rustgram.lnk";

/// COM CLSID used for Rustgram's `INotificationActivationCallback` toast activator.
///
/// Distinct from official Telegram Desktop's CLSID so both can coexist safely.
pub const TOAST_ACTIVATOR_CLSID_STR: &str = "{8B42D1E6-5C79-4E3A-9D14-7A3F2E8B61C9}";

/// OS-independent representation of a Windows unique file identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UniqueFileId {
    /// Volume serial number (`dwVolumeSerialNumber`).
    pub volume_serial: u32,
    /// Packed 64-bit file index matching tdesktop's `(nFileIndexLow << 32) | nFileIndexHigh`.
    pub file_index: u64,
}

/// Configuration for creating or validating a Start-menu `.lnk` shortcut for toasts.
#[derive(Debug, Clone)]
pub struct ShortcutConfig {
    /// `System.AppUserModel.ID` value written into the shortcut property store.
    pub app_user_model_id: String,
    /// Shortcut filename (e.g. `"Rustgram.lnk"`).
    pub shortcut_name: String,
    /// Target executable path.
    pub executable_path: PathBuf,
    /// Working directory for the shortcut.
    pub working_dir: PathBuf,
    /// Optional command-line arguments.
    pub arguments: String,
    /// Optional description / tooltip text.
    pub description: String,
    /// Optional `System.AppUserModel.ToastActivatorCLSID` (formatted `{XXXXXXXX-...}`).
    pub toast_activator_clsid: Option<String>,
    /// Whether to set `System.AppUserModel.StartPinOption = NOPINONINSTALL`.
    pub no_pin_on_install: bool,
}

impl ShortcutConfig {
    /// Build a default Start-menu shortcut configuration for Rustgram.
    pub fn for_executable(executable_path: PathBuf, working_dir: PathBuf) -> Self {
        Self {
            app_user_model_id: DEFAULT_APP_USER_MODEL_ID.to_string(),
            shortcut_name: DEFAULT_SHORTCUT_NAME.to_string(),
            executable_path,
            working_dir,
            arguments: String::new(),
            description: "Rustgram".to_string(),
            toast_activator_clsid: Some(TOAST_ACTIVATOR_CLSID_STR.to_string()),
            no_pin_on_install: true,
        }
    }
}

/// Compose an AppUserModelID, appending `.<instance_hash>` for portable instances
/// (matching `AppUserModelId::Id()` in `windows_app_user_model_id.cpp`).
pub fn build_app_user_model_id(base: &str, instance_hash: Option<&str>) -> String {
    match instance_hash {
        Some(hash) if !hash.is_empty() => format!("{base}.{hash}"),
        _ => base.to_string(),
    }
}

/// Return the absolute path to the currently running executable.
pub fn current_executable_path() -> Result<PathBuf> {
    std::env::current_exe().map_err(Into::into)
}

/// Query the unique file identifier of `path` (`GetFileInformationByHandle` on Windows).
pub fn get_unique_file_id(path: &Path) -> Result<UniqueFileId> {
    #[cfg(windows)]
    {
        win_impl::get_unique_file_id(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Set the explicit AppUserModelID for the current process.
///
/// Must be called early during startup before creating windows or toast notifications.
pub fn set_current_process_app_id(aumid: &str) -> Result<()> {
    #[cfg(windows)]
    {
        win_impl::set_current_process_app_id(aumid)
    }
    #[cfg(not(windows))]
    {
        let _ = aumid;
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Query the explicit AppUserModelID of the current process.
pub fn get_current_process_app_id() -> Result<String> {
    #[cfg(windows)]
    {
        win_impl::get_current_process_app_id()
    }
    #[cfg(not(windows))]
    {
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Return the current user's Start Menu Programs directory
/// (`%APPDATA%\Microsoft\Windows\Start Menu\Programs`).
pub fn start_menu_programs_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        win_impl::start_menu_programs_dir()
    }
    #[cfg(not(windows))]
    {
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Validate an existing `.lnk` shortcut at `shortcut_path` against `config`.
///
/// If the shortcut points to the same executable (`UniqueFileId`) and has matching
/// `AppUserModel.ID` (and `ToastActivatorCLSID` when configured), returns `Ok(true)`.
/// If those properties are empty on a matching shortcut, updates them in place and returns `Ok(true)`.
pub fn validate_shortcut_at(shortcut_path: &Path, config: &ShortcutConfig) -> Result<bool> {
    #[cfg(windows)]
    {
        win_impl::validate_shortcut_at(shortcut_path, config)
    }
    #[cfg(not(windows))]
    {
        let _ = (shortcut_path, config);
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Validate or create the Start-menu shortcut inside `shortcut_dir`.
///
/// Takes `shortcut_dir` explicitly so tests can target a temporary directory without
/// modifying the real user's Start Menu.
pub fn validate_or_create_shortcut_in(
    shortcut_dir: &Path,
    config: &ShortcutConfig,
) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        win_impl::validate_or_create_shortcut_in(shortcut_dir, config)
    }
    #[cfg(not(windows))]
    {
        let _ = (shortcut_dir, config);
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Remove `<shortcut_dir>/<shortcut_name>` if it exists and targets `executable_path`.
///
/// Returns `Ok(true)` if the shortcut was removed, `Ok(false)` if absent or pointing elsewhere.
pub fn cleanup_shortcut_in(
    shortcut_dir: &Path,
    shortcut_name: &str,
    executable_path: &Path,
) -> Result<bool> {
    #[cfg(windows)]
    {
        win_impl::cleanup_shortcut_in(shortcut_dir, shortcut_name, executable_path)
    }
    #[cfg(not(windows))]
    {
        let _ = (shortcut_dir, shortcut_name, executable_path);
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Parse a braced or unbraced GUID string like `"{8B42D1E6-5C79-4E3A-9D14-7A3F2E8B61C9}"`.
pub fn parse_guid_bytes(s: &str) -> Option<(u32, u16, u16, [u8; 8])> {
    let trimmed = s.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = trimmed.split('-').collect();
    if parts.len() != 5 {
        return None;
    }
    if parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return None;
    }
    let d1 = u32::from_str_radix(parts[0], 16).ok()?;
    let d2 = u16::from_str_radix(parts[1], 16).ok()?;
    let d3 = u16::from_str_radix(parts[2], 16).ok()?;
    let d4_hi = u16::from_str_radix(parts[3], 16).ok()?;
    let d4_lo = u64::from_str_radix(parts[4], 16).ok()?;
    let mut d4 = [0u8; 8];
    d4[0..2].copy_from_slice(&d4_hi.to_be_bytes());
    d4[2..8].copy_from_slice(&d4_lo.to_be_bytes()[2..8]);
    Some((d1, d2, d3, d4))
}

#[cfg(windows)]
pub(crate) mod win_impl {
    use super::*;
    use crate::error::PlatformError;
    use std::ffi::OsStr;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, PROPERTYKEY};
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        InitPropVariantFromCLSID, PROPVARIANT, PropVariantToGUID, PropVariantToStringAlloc,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoIncrementMTAUsage,
        CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READWRITE,
    };
    use windows::Win32::System::Variant::VT_EMPTY;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        FOLDERID_Programs, GetCurrentProcessExplicitAppUserModelID, IShellLinkW, KF_FLAG_CREATE,
        SHGetKnownFolderPath, SetCurrentProcessExplicitAppUserModelID, ShellLink,
    };
    use windows::core::{GUID, Interface, PCWSTR};

    /// `System.AppUserModel.ID` property key.
    pub(crate) const PKEY_APPUSERMODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
        pid: 5,
    };

    /// `System.AppUserModel.StartPinOption` property key.
    pub(crate) const PKEY_APPUSERMODEL_START_PIN_OPTION: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
        pid: 12,
    };

    /// `System.AppUserModel.ToastActivatorCLSID` property key.
    pub(crate) const PKEY_APPUSERMODEL_TOAST_ACTIVATOR_CLSID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
        pid: 26,
    };

    /// `APPUSERMODEL_STARTPINOPTION_NOPINONINSTALL`.
    pub(crate) const APPUSERMODEL_STARTPINOPTION_NOPINONINSTALL: u32 = 1;

    /// Convert any OS string to a null-terminated UTF-16 vector.
    pub(crate) fn to_wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Convert a `GUID` from string representation.
    pub(crate) fn guid_from_str(s: &str) -> Result<GUID> {
        let (d1, d2, d3, d4) = parse_guid_bytes(s)
            .ok_or_else(|| PlatformError::InvalidArgument(format!("invalid GUID: {s}")))?;
        Ok(GUID::from_values(d1, d2, d3, d4))
    }

    /// Pin the process-wide COM multi-threaded apartment (MTA) for the lifetime of the process.
    ///
    /// `windows-core` caches agile WinRT activation factories in process-global `FactoryCache`
    /// statics and intentionally leaks them. If a thread calls `CoInitializeEx` before the first
    /// WinRT activation and later calls `CoUninitialize`, the process MTA usage count can drop to
    /// zero and unload the WinRT server DLLs while `FactoryCache` still holds raw pointers to them.
    /// Calling `CoIncrementMTAUsage` once (and intentionally never decrementing it, matching
    /// `windows-core`'s own `load_factory`) keeps the process MTA alive without changing the
    /// calling thread's apartment (STA threads remain STA).
    pub(crate) fn ensure_process_mta() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            // SAFETY: `CoIncrementMTAUsage` is safe to call from any thread (STA, MTA, or
            // uninitialized) and does not alter the calling thread's COM apartment. The returned
            // cookie is intentionally leaked so the process MTA stays alive for `FactoryCache`.
            let _ = unsafe { CoIncrementMTAUsage() };
        });
    }

    /// RAII guard for COM initialization on the current thread.
    pub(crate) struct ComGuard {
        should_uninit: bool,
    }

    impl ComGuard {
        pub(crate) fn init_sta() -> Self {
            // SAFETY: Calling CoInitializeEx with STA is safe on any thread; if the thread
            // already initialized COM with a different apartment model (RPC_E_CHANGED_MODE),
            // we simply don't balance with CoUninitialize.
            let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            Self {
                should_uninit: hr.is_ok(),
            }
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.should_uninit {
                // SAFETY: Paired with a successful CoInitializeEx on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    pub(crate) fn get_unique_file_id(path: &Path) -> Result<UniqueFileId> {
        let wide = to_wide(path);
        // SAFETY: CreateFileW with desired_access = 0 opens a metadata-only handle that does
        // not lock file contents; handle is closed before returning.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|e| PlatformError::from_win("CreateFileW", e))?;

        if handle == INVALID_HANDLE_VALUE {
            return Err(PlatformError::last_win32("CreateFileW"));
        }

        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `handle` is valid and `info` is a properly initialized out-param.
        let res = unsafe { GetFileInformationByHandle(handle, &mut info) };
        // SAFETY: Closing our owned handle.
        let _ = unsafe { CloseHandle(handle) };
        res.map_err(|e| PlatformError::from_win("GetFileInformationByHandle", e))?;

        Ok(UniqueFileId {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: (u64::from(info.nFileIndexLow) << 32) | u64::from(info.nFileIndexHigh),
        })
    }

    pub(crate) fn set_current_process_app_id(aumid: &str) -> Result<()> {
        if aumid.is_empty() {
            return Err(PlatformError::InvalidArgument(
                "AppUserModelID cannot be empty".into(),
            ));
        }
        let wide = to_wide(aumid);
        // SAFETY: `wide` is null-terminated UTF-16 valid for the duration of the call.
        unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(wide.as_ptr())) }
            .map_err(|e| PlatformError::from_win("SetCurrentProcessExplicitAppUserModelID", e))
    }

    pub(crate) fn get_current_process_app_id() -> Result<String> {
        // SAFETY: Windows allocates the returned PWSTR with CoTaskMemAlloc; we copy and free it.
        let ptr = unsafe { GetCurrentProcessExplicitAppUserModelID() }
            .map_err(|e| PlatformError::from_win("GetCurrentProcessExplicitAppUserModelID", e))?;
        let text = unsafe { ptr.to_string() }
            .map_err(|e| PlatformError::InvalidArgument(format!("invalid UTF-16 AUMID: {e}")));
        // SAFETY: `ptr` was allocated by GetCurrentProcessExplicitAppUserModelID.
        unsafe { CoTaskMemFree(Some(ptr.0 as *const _)) };
        text
    }

    pub(crate) fn known_folder_path(folder_id: &GUID) -> Result<PathBuf> {
        // SAFETY: SHGetKnownFolderPath returns a CoTaskMem-allocated PWSTR which we free.
        let ptr = unsafe { SHGetKnownFolderPath(folder_id, KF_FLAG_CREATE, None) }
            .map_err(|e| PlatformError::from_win("SHGetKnownFolderPath", e))?;
        let path = unsafe { ptr.to_string() }
            .map(PathBuf::from)
            .map_err(|e| PlatformError::InvalidArgument(format!("invalid UTF-16 path: {e}")));
        // SAFETY: Freeing the string allocated by SHGetKnownFolderPath.
        unsafe { CoTaskMemFree(Some(ptr.0 as *const _)) };
        path
    }

    pub(crate) fn start_menu_programs_dir() -> Result<PathBuf> {
        known_folder_path(&FOLDERID_Programs)
    }

    /// Create and save a Windows `.lnk` shortcut with optional AUMID, StartPinOption,
    /// and ToastActivatorCLSID properties.
    pub(crate) fn save_shell_link(shortcut_path: &Path, config: &ShortcutConfig) -> Result<()> {
        let _com = ComGuard::init_sta();

        if let Some(parent) = shortcut_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // SAFETY: COM is initialized; ShellLink is a standard in-proc Windows Shell class.
        let shell_link: IShellLinkW = unsafe {
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| PlatformError::from_win("CoCreateInstance(ShellLink)", e))?
        };

        let exe_w = to_wide(&config.executable_path);
        let args_w = to_wide(&config.arguments);
        let work_w = to_wide(&config.working_dir);
        let desc_w = to_wide(&config.description);

        // SAFETY: All wide strings are null-terminated and live through each COM call.
        unsafe {
            shell_link
                .SetPath(PCWSTR(exe_w.as_ptr()))
                .map_err(|e| PlatformError::from_win("IShellLinkW::SetPath", e))?;
            shell_link
                .SetArguments(PCWSTR(args_w.as_ptr()))
                .map_err(|e| PlatformError::from_win("IShellLinkW::SetArguments", e))?;
            shell_link
                .SetWorkingDirectory(PCWSTR(work_w.as_ptr()))
                .map_err(|e| PlatformError::from_win("IShellLinkW::SetWorkingDirectory", e))?;
            if !config.description.is_empty() {
                shell_link
                    .SetDescription(PCWSTR(desc_w.as_ptr()))
                    .map_err(|e| PlatformError::from_win("IShellLinkW::SetDescription", e))?;
            }
        }

        let prop_store: IPropertyStore = shell_link
            .cast()
            .map_err(|e| PlatformError::from_win("QueryInterface(IPropertyStore)", e))?;

        // SAFETY: PROPVARIANT values are constructed via windows-rs / Windows APIs and dropped cleanly.
        unsafe {
            if !config.app_user_model_id.is_empty() {
                let aumid_pv = PROPVARIANT::from(config.app_user_model_id.as_str());
                prop_store
                    .SetValue(&PKEY_APPUSERMODEL_ID, &aumid_pv)
                    .map_err(|e| PlatformError::from_win("IPropertyStore::SetValue(AUMID)", e))?;
            }

            if config.no_pin_on_install {
                let pin_pv = PROPVARIANT::from(APPUSERMODEL_STARTPINOPTION_NOPINONINSTALL);
                prop_store
                    .SetValue(&PKEY_APPUSERMODEL_START_PIN_OPTION, &pin_pv)
                    .map_err(|e| {
                        PlatformError::from_win("IPropertyStore::SetValue(StartPinOption)", e)
                    })?;
            }

            if let Some(ref clsid_str) = config.toast_activator_clsid {
                let guid = guid_from_str(clsid_str)?;
                let clsid_pv = InitPropVariantFromCLSID(&guid)
                    .map_err(|e| PlatformError::from_win("InitPropVariantFromCLSID", e))?;
                prop_store
                    .SetValue(&PKEY_APPUSERMODEL_TOAST_ACTIVATOR_CLSID, &clsid_pv)
                    .map_err(|e| {
                        PlatformError::from_win("IPropertyStore::SetValue(ToastActivator)", e)
                    })?;
            }

            prop_store
                .Commit()
                .map_err(|e| PlatformError::from_win("IPropertyStore::Commit", e))?;
        }

        let persist_file: IPersistFile = shell_link
            .cast()
            .map_err(|e| PlatformError::from_win("QueryInterface(IPersistFile)", e))?;
        let lnk_w = to_wide(shortcut_path);
        // SAFETY: `lnk_w` is null-terminated UTF-16.
        unsafe {
            persist_file
                .Save(PCWSTR(lnk_w.as_ptr()), true)
                .map_err(|e| PlatformError::from_win("IPersistFile::Save", e))?;
        }

        Ok(())
    }

    pub(crate) fn validate_shortcut_at(
        shortcut_path: &Path,
        config: &ShortcutConfig,
    ) -> Result<bool> {
        if !shortcut_path.exists() {
            return Ok(false);
        }

        let target_id = match get_unique_file_id(&config.executable_path) {
            Ok(id) => id,
            Err(_) => return Ok(false),
        };

        let _com = ComGuard::init_sta();

        // SAFETY: COM initialized on this thread.
        let shell_link: IShellLinkW = unsafe {
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| PlatformError::from_win("CoCreateInstance(ShellLink)", e))?
        };
        let persist_file: IPersistFile = shell_link
            .cast()
            .map_err(|e| PlatformError::from_win("QueryInterface(IPersistFile)", e))?;
        let lnk_w = to_wide(shortcut_path);
        // SAFETY: Loading existing .lnk file with read/write mode to allow updating empty properties.
        if unsafe { persist_file.Load(PCWSTR(lnk_w.as_ptr()), STGM_READWRITE) }.is_err() {
            return Ok(false);
        }

        let mut got_path = vec![0u16; 1024];
        // SAFETY: `got_path` buffer length passed accurately to GetPath.
        if unsafe { shell_link.GetPath(&mut got_path, std::ptr::null_mut(), 0) }.is_err() {
            return Ok(false);
        }
        let nul_pos = got_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(got_path.len());
        let resolved = PathBuf::from(std::ffi::OsString::from_wide(&got_path[..nul_pos]));
        if get_unique_file_id(&resolved).ok() != Some(target_id) {
            return Ok(false);
        }

        let prop_store: IPropertyStore = shell_link
            .cast()
            .map_err(|e| PlatformError::from_win("QueryInterface(IPropertyStore)", e))?;

        // SAFETY: Reading PKEY_APPUSERMODEL_ID and PKEY_APPUSERMODEL_TOAST_ACTIVATOR_CLSID.
        let app_id_pv = unsafe { prop_store.GetValue(&PKEY_APPUSERMODEL_ID) }
            .map_err(|e| PlatformError::from_win("IPropertyStore::GetValue(AUMID)", e))?;

        let existing_aumid = unsafe { PropVariantToStringAlloc(&app_id_pv) }
            .ok()
            .map(|p| {
                let s = unsafe { p.to_string() }.unwrap_or_default();
                unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
                s
            });
        let aumid_empty = app_id_pv.vt() == VT_EMPTY;
        let good1 = existing_aumid.as_deref() == Some(config.app_user_model_id.as_str());
        let bad1 = !good1 && !aumid_empty;

        let (good2, bad2) = if let Some(ref expected_clsid) = config.toast_activator_clsid {
            let expected_guid = guid_from_str(expected_clsid)?;
            let act_pv = unsafe { prop_store.GetValue(&PKEY_APPUSERMODEL_TOAST_ACTIVATOR_CLSID) }
                .map_err(|e| {
                PlatformError::from_win("IPropertyStore::GetValue(ToastActivator)", e)
            })?;
            let clsid_empty = act_pv.vt() == VT_EMPTY;
            let got_guid = unsafe { PropVariantToGUID(&act_pv) }.ok();
            let g2 = got_guid == Some(expected_guid);
            (g2, !g2 && !clsid_empty)
        } else {
            (true, false)
        };

        if good1 && good2 {
            return Ok(true);
        }
        if bad1 || bad2 {
            return Ok(false);
        }

        // Properties were empty — populate and commit, matching windows_app_user_model_id.cpp.
        // SAFETY: Setting missing properties on the loaded property store.
        unsafe {
            let aumid_pv = PROPVARIANT::from(config.app_user_model_id.as_str());
            prop_store
                .SetValue(&PKEY_APPUSERMODEL_ID, &aumid_pv)
                .map_err(|e| PlatformError::from_win("IPropertyStore::SetValue(AUMID)", e))?;
            if let Some(ref clsid_str) = config.toast_activator_clsid {
                let guid = guid_from_str(clsid_str)?;
                let clsid_pv = InitPropVariantFromCLSID(&guid)
                    .map_err(|e| PlatformError::from_win("InitPropVariantFromCLSID", e))?;
                prop_store
                    .SetValue(&PKEY_APPUSERMODEL_TOAST_ACTIVATOR_CLSID, &clsid_pv)
                    .map_err(|e| {
                        PlatformError::from_win("IPropertyStore::SetValue(ToastActivator)", e)
                    })?;
            }
            prop_store
                .Commit()
                .map_err(|e| PlatformError::from_win("IPropertyStore::Commit", e))?;
            if persist_file.IsDirty().is_ok() {
                persist_file
                    .Save(PCWSTR(lnk_w.as_ptr()), true)
                    .map_err(|e| PlatformError::from_win("IPersistFile::Save", e))?;
            }
        }

        Ok(true)
    }

    pub(crate) fn validate_or_create_shortcut_in(
        shortcut_dir: &Path,
        config: &ShortcutConfig,
    ) -> Result<PathBuf> {
        let path = shortcut_dir.join(&config.shortcut_name);
        if validate_shortcut_at(&path, config)? {
            return Ok(path);
        }
        save_shell_link(&path, config)?;
        Ok(path)
    }

    pub(crate) fn cleanup_shortcut_in(
        shortcut_dir: &Path,
        shortcut_name: &str,
        executable_path: &Path,
    ) -> Result<bool> {
        let path = shortcut_dir.join(shortcut_name);
        if !path.exists() {
            return Ok(false);
        }
        let my_id = match get_unique_file_id(executable_path) {
            Ok(id) => id,
            Err(_) => return Ok(false),
        };

        let _com = ComGuard::init_sta();
        // SAFETY: COM initialized on this thread.
        let shell_link: IShellLinkW = unsafe {
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| PlatformError::from_win("CoCreateInstance(ShellLink)", e))?
        };
        let persist_file: IPersistFile = shell_link
            .cast()
            .map_err(|e| PlatformError::from_win("QueryInterface(IPersistFile)", e))?;
        let lnk_w = to_wide(&path);
        // SAFETY: Reading existing shortcut target.
        if unsafe { persist_file.Load(PCWSTR(lnk_w.as_ptr()), STGM_READWRITE) }.is_err() {
            return Ok(false);
        }
        let mut got_path = vec![0u16; 1024];
        // SAFETY: Valid buffer passed to GetPath.
        if unsafe { shell_link.GetPath(&mut got_path, std::ptr::null_mut(), 0) }.is_err() {
            return Ok(false);
        }
        let nul_pos = got_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(got_path.len());
        let resolved = PathBuf::from(std::ffi::OsString::from_wide(&got_path[..nul_pos]));
        drop(persist_file);
        drop(shell_link);

        if get_unique_file_id(&resolved).ok() == Some(my_id) {
            std::fs::remove_file(&path)?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aumid_portable_suffix() {
        assert_eq!(build_app_user_model_id("Rustgram", None), "Rustgram");
        assert_eq!(build_app_user_model_id("Rustgram", Some("")), "Rustgram");
        assert_eq!(
            build_app_user_model_id("Rustgram", Some("deadbeef")),
            "Rustgram.deadbeef"
        );
    }

    #[test]
    fn parse_guid_valid_and_invalid() {
        let parsed = parse_guid_bytes(TOAST_ACTIVATOR_CLSID_STR);
        assert!(parsed.is_some());
        let (d1, d2, d3, d4) = parsed.unwrap();
        assert_eq!(d1, 0x8B42D1E6);
        assert_eq!(d2, 0x5C79);
        assert_eq!(d3, 0x4E3A);
        assert_eq!(d4, [0x9D, 0x14, 0x7A, 0x3F, 0x2E, 0x8B, 0x61, 0xC9]);

        assert!(parse_guid_bytes("not-a-guid").is_none());
        assert!(parse_guid_bytes("{1234}").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn shortcut_create_validate_and_cleanup_in_temp_dir() {
        let tmp =
            std::env::temp_dir().join(format!("rustgram_shortcut_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let exe = current_executable_path().unwrap();
        let cfg = ShortcutConfig::for_executable(exe.clone(), tmp.clone());

        let created = validate_or_create_shortcut_in(&tmp, &cfg).unwrap();
        assert!(created.exists());
        assert!(validate_shortcut_at(&created, &cfg).unwrap());

        // Different AUMID must fail validation.
        let mut wrong_cfg = cfg.clone();
        wrong_cfg.app_user_model_id = "OtherApp".to_string();
        assert!(!validate_shortcut_at(&created, &wrong_cfg).unwrap());

        // Cleanup removes shortcut targeting our executable.
        assert!(cleanup_shortcut_in(&tmp, &cfg.shortcut_name, &exe).unwrap());
        assert!(!created.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
