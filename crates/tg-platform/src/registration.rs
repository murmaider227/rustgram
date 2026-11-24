//! URL scheme registration (`tg://`, `tonsite://`, `ton://`), autostart, and SendTo shortcuts.
//!
//! Port of `Telegram/lib_base/base/platform/win/base_url_scheme_win.cpp`,
//! `Telegram/SourceFiles/core/application.cpp` (`RegisterUrlScheme`), and
//! `Telegram/SourceFiles/platform/win/specific_win.cpp` (`ManageAppLink`,
//! `AutostartToggle`, `psSendToMenu`).

use std::path::{Path, PathBuf};

use crate::app_id::DEFAULT_APP_USER_MODEL_ID;
use crate::error::{PlatformError, Result};

/// Windows registry hive selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryHive {
    /// `HKEY_CURRENT_USER`.
    CurrentUser,
    /// `HKEY_LOCAL_MACHINE`.
    LocalMachine,
}

/// Root location in the Windows Registry.
///
/// Production code uses [`RegistryRoot::hkcu()`]. Tests construct an isolated
/// subtree via [`RegistryRoot::hkcu_subkey`] so real user registry keys are never touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRoot {
    /// Registry hive (`HKCU` or `HKLM`).
    pub hive: RegistryHive,
    /// Optional subkey prefix prepended to all relative key paths (empty for real root).
    pub prefix: String,
}

impl RegistryRoot {
    /// Real `HKEY_CURRENT_USER` root.
    pub fn hkcu() -> Self {
        Self {
            hive: RegistryHive::CurrentUser,
            prefix: String::new(),
        }
    }

    /// Real `HKEY_LOCAL_MACHINE` root.
    pub fn hklm() -> Self {
        Self {
            hive: RegistryHive::LocalMachine,
            prefix: String::new(),
        }
    }

    /// Isolated subtree under `HKEY_CURRENT_USER\<prefix>` for safe testing.
    pub fn hkcu_subkey(prefix: impl Into<String>) -> Self {
        let p = prefix.into().trim_matches('\\').to_string();
        Self {
            hive: RegistryHive::CurrentUser,
            prefix: p,
        }
    }

    /// Resolve a relative registry subkey path against `self.prefix`.
    pub fn resolve(&self, subkey: &str) -> String {
        let clean = subkey.trim_start_matches('\\');
        if self.prefix.is_empty() {
            clean.to_string()
        } else if clean.is_empty() {
            self.prefix.clone()
        } else {
            format!("{}\\{}", self.prefix, clean)
        }
    }

    /// Recursively delete the entire test subtree rooted at `self.prefix`.
    ///
    /// Refuses to run if `self.prefix` is empty so real hives can never be wiped.
    pub fn delete_tree(&self) -> Result<()> {
        if self.prefix.is_empty() {
            return Err(PlatformError::InvalidArgument(
                "refusing to delete an empty registry prefix".into(),
            ));
        }
        #[cfg(windows)]
        {
            win_reg::delete_tree(self.hive, &self.prefix)
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::Unsupported)
        }
    }
}

/// Descriptor for registering a custom URL scheme in Windows Registry.
///
/// Mirrors `base::Platform::UrlSchemeDescriptor` from `base_url_scheme_win.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlSchemeDescriptor {
    /// Path to the application executable.
    pub executable: PathBuf,
    /// Extra command-line arguments (e.g. `""` or `"-workdir \"C:\\path\""`).
    pub arguments: String,
    /// Protocol scheme name without `://` (e.g. `"tg"`, `"tonsite"`, `"ton"`).
    pub protocol: String,
    /// Protocol display name written as `"URL:<protocol_name>"`.
    pub protocol_name: String,
    /// Short ProgID prefix (e.g. `"rustgram"` -> `"rustgram.tg"`).
    pub short_app_name: String,
    /// Registry application key under `Software\<long_app_name>\Capabilities`.
    pub long_app_name: String,
    /// Application display name registered under `Software\RegisteredApplications`.
    pub display_app_name: String,
    /// Application description under `Capabilities\ApplicationDescription`.
    pub display_app_description: String,
}

impl UrlSchemeDescriptor {
    /// Build a descriptor for the `tg://` scheme adapted to Rustgram.
    pub fn tg(executable: PathBuf, custom_workdir: Option<&Path>) -> Self {
        Self::for_protocol(executable, custom_workdir, "tg", "Rustgram Link")
    }

    /// Build a descriptor for the `tonsite://` scheme adapted to Rustgram.
    pub fn tonsite(executable: PathBuf, custom_workdir: Option<&Path>) -> Self {
        Self::for_protocol(executable, custom_workdir, "tonsite", "TonSite Link")
    }

    /// Build a descriptor for the `ton://` scheme adapted to Rustgram.
    pub fn ton(executable: PathBuf, custom_workdir: Option<&Path>) -> Self {
        Self::for_protocol(executable, custom_workdir, "ton", "Ton Link")
    }

    /// Build a Rustgram URL scheme descriptor for an arbitrary protocol.
    pub fn for_protocol(
        executable: PathBuf,
        custom_workdir: Option<&Path>,
        protocol: impl Into<String>,
        protocol_name: impl Into<String>,
    ) -> Self {
        let arguments = match custom_workdir {
            Some(dir) => format!("-workdir \"{}\"", dir.display()),
            None => String::new(),
        };
        Self {
            executable,
            arguments,
            protocol: protocol.into(),
            protocol_name: protocol_name.into(),
            short_app_name: "rustgram".to_string(),
            long_app_name: "Rustgram".to_string(),
            display_app_name: "Rustgram".to_string(),
            display_app_description: "Rustgram".to_string(),
        }
    }

    /// Format the exact shell open command string matching `base_url_scheme_win.cpp`.
    pub fn open_command(&self) -> String {
        let exe = normalize_win_separators(&self.executable);
        format!("\"{}\" {} -- \"%1\"", exe, self.arguments)
    }

    /// Format the exact `DefaultIcon` value matching `base_url_scheme_win.cpp`.
    pub fn default_icon_value(&self) -> String {
        let exe = normalize_win_separators(&self.executable);
        format!("\"{exe},1\"")
    }

    /// Format the ProgID `<short_app_name>.<protocol>`.
    pub fn named_protocol(&self) -> String {
        format!("{}.{}", self.short_app_name, self.protocol)
    }
}

fn normalize_win_separators(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

/// Check whether a URL scheme is currently registered with the exact keys/values of `desc`.
pub fn check_url_scheme(root: &RegistryRoot, desc: &UrlSchemeDescriptor) -> Result<bool> {
    #[cfg(windows)]
    {
        win_reg::full_register(root, desc, false)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, desc);
        Err(PlatformError::Unsupported)
    }
}

/// Register a URL scheme under `root` writing both Default Programs (`Capabilities` /
/// `RegisteredApplications` / `<short>.<protocol>`) and legacy `Software\Classes\<protocol>` keys.
pub fn register_url_scheme(root: &RegistryRoot, desc: &UrlSchemeDescriptor) -> Result<()> {
    #[cfg(windows)]
    {
        let ok = win_reg::full_register(root, desc, true)?;
        if ok {
            Ok(())
        } else {
            Err(PlatformError::Registry(format!(
                "failed to register URL scheme '{}'",
                desc.protocol
            )))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (root, desc);
        Err(PlatformError::Unsupported)
    }
}

/// Unregister a URL scheme under `root` if it is currently registered to `desc`
/// (matching `UnregisterUrlScheme` in `base_url_scheme_win.cpp`).
///
/// Returns `Ok(true)` if the scheme matched and was removed, `Ok(false)` if not owned by `desc`.
pub fn unregister_url_scheme(root: &RegistryRoot, desc: &UrlSchemeDescriptor) -> Result<bool> {
    #[cfg(windows)]
    {
        if !check_url_scheme(root, desc)? {
            return Ok(false);
        }
        win_reg::unregister_default_program(root, desc);
        win_reg::unregister_legacy(root, desc);
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, desc);
        Err(PlatformError::Unsupported)
    }
}

/// Register both `tg://` and `tonsite://` schemes under `root`.
pub fn register_default_schemes(
    root: &RegistryRoot,
    executable: &Path,
    custom_workdir: Option<&Path>,
) -> Result<()> {
    register_url_scheme(
        root,
        &UrlSchemeDescriptor::tg(executable.to_path_buf(), custom_workdir),
    )?;
    register_url_scheme(
        root,
        &UrlSchemeDescriptor::tonsite(executable.to_path_buf(), custom_workdir),
    )?;
    Ok(())
}

/// Unregister both `tg://` and `tonsite://` schemes under `root` when owned by this executable.
pub fn unregister_default_schemes(
    root: &RegistryRoot,
    executable: &Path,
    custom_workdir: Option<&Path>,
) -> Result<()> {
    let _ = unregister_url_scheme(
        root,
        &UrlSchemeDescriptor::tg(executable.to_path_buf(), custom_workdir),
    )?;
    let _ = unregister_url_scheme(
        root,
        &UrlSchemeDescriptor::tonsite(executable.to_path_buf(), custom_workdir),
    )?;
    Ok(())
}

/// Configuration for Startup and SendTo `.lnk` shortcuts.
#[derive(Debug, Clone)]
pub struct AppLinkConfig {
    /// Shortcut filename without directory (e.g. `"Rustgram.lnk"`).
    pub shortcut_name: String,
    /// Path to the application executable.
    pub executable: PathBuf,
    /// Working directory set on the shortcut.
    pub working_dir: PathBuf,
    /// `System.AppUserModel.ID` property written onto the shortcut.
    pub app_user_model_id: String,
}

impl AppLinkConfig {
    /// Create a default configuration for Rustgram.
    pub fn for_executable(executable: PathBuf, working_dir: PathBuf) -> Self {
        Self {
            shortcut_name: "Rustgram.lnk".to_string(),
            executable,
            working_dir,
            app_user_model_id: DEFAULT_APP_USER_MODEL_ID.to_string(),
        }
    }
}

/// Return the current user's Windows Startup folder (`FOLDERID_Startup`).
pub fn startup_folder_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        crate::app_id::win_impl::known_folder_path(&windows::Win32::UI::Shell::FOLDERID_Startup)
    }
    #[cfg(not(windows))]
    {
        Err(PlatformError::Unsupported)
    }
}

/// Return the current user's Windows SendTo folder (`FOLDERID_SendTo`).
pub fn send_to_folder_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        crate::app_id::win_impl::known_folder_path(&windows::Win32::UI::Shell::FOLDERID_SendTo)
    }
    #[cfg(not(windows))]
    {
        Err(PlatformError::Unsupported)
    }
}

/// Create or remove the autostart shortcut in `startup_dir` with `-autostart`.
///
/// Mirrors `AutostartToggle` / `ManageAppLink(FOLDERID_Startup, L"-autostart", ...)`
/// in `specific_win.cpp`.
pub fn set_autostart_in(startup_dir: &Path, config: &AppLinkConfig, enabled: bool) -> Result<()> {
    manage_app_link_in(
        startup_dir,
        config,
        enabled,
        "-autostart",
        "Rustgram autorun link.\nYou can disable autorun in Rustgram settings.",
    )
}

/// Check whether the autostart shortcut exists inside `startup_dir`.
pub fn is_autostart_enabled_in(startup_dir: &Path, shortcut_name: &str) -> bool {
    startup_dir.join(shortcut_name).exists()
}

/// Create or remove the Explorer "Send to" shortcut in `send_to_dir` with `--`.
///
/// Mirrors `psSendToMenu` / `ManageAppLink(FOLDERID_SendTo, L"--", ...)`
/// in `specific_win.cpp`.
pub fn set_send_to_in(send_to_dir: &Path, config: &AppLinkConfig, enabled: bool) -> Result<()> {
    manage_app_link_in(
        send_to_dir,
        config,
        enabled,
        "--",
        "Rustgram send to link.\nYou can disable send to menu item in Rustgram settings.",
    )
}

/// Check whether the "Send to" shortcut exists inside `send_to_dir`.
pub fn is_send_to_enabled_in(send_to_dir: &Path, shortcut_name: &str) -> bool {
    send_to_dir.join(shortcut_name).exists()
}

fn manage_app_link_in(
    folder: &Path,
    config: &AppLinkConfig,
    create: bool,
    args: &str,
    description: &str,
) -> Result<()> {
    let lnk = folder.join(&config.shortcut_name);
    if !create {
        if lnk.exists() {
            std::fs::remove_file(&lnk)?;
        }
        return Ok(());
    }
    #[cfg(windows)]
    {
        let sc = crate::app_id::ShortcutConfig {
            app_user_model_id: config.app_user_model_id.clone(),
            shortcut_name: config.shortcut_name.clone(),
            executable_path: config.executable.clone(),
            working_dir: config.working_dir.clone(),
            arguments: args.to_string(),
            description: description.to_string(),
            toast_activator_clsid: None,
            no_pin_on_install: false,
        };
        crate::app_id::win_impl::save_shell_link(&lnk, &sc)
    }
    #[cfg(not(windows))]
    {
        let _ = (args, description);
        Err(PlatformError::Unsupported)
    }
}

#[cfg(windows)]
pub(crate) mod win_reg {
    use super::*;
    use crate::app_id::win_impl::to_wide;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_WRITE,
        REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW,
        RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryInfoKeyW,
        RegQueryValueExW, RegSetValueExW,
    };
    use windows::core::PCWSTR;

    pub(crate) fn root_hkey(hive: RegistryHive) -> HKEY {
        match hive {
            RegistryHive::CurrentUser => HKEY_CURRENT_USER,
            RegistryHive::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }

    /// RAII wrapper around an opened Windows `HKEY`.
    pub(crate) struct OwnedRegKey(pub(crate) HKEY);

    impl Drop for OwnedRegKey {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: Closing our owned registry handle once.
                let _ = unsafe { RegCloseKey(self.0) };
            }
        }
    }

    pub(crate) fn open_reg_key(
        root: &RegistryRoot,
        subkey: &str,
        write: bool,
    ) -> Result<Option<OwnedRegKey>> {
        let full = root.resolve(subkey);
        let wide = to_wide(&full);
        let hroot = root_hkey(root.hive);
        let mut hkey = HKEY::default();
        let access = if write {
            KEY_QUERY_VALUE | KEY_WRITE
        } else {
            KEY_READ
        };

        // SAFETY: `wide` is null-terminated UTF-16; `hkey` receives the opened handle.
        let status =
            unsafe { RegOpenKeyExW(hroot, PCWSTR(wide.as_ptr()), Some(0), access, &mut hkey) };
        if status == ERROR_SUCCESS {
            return Ok(Some(OwnedRegKey(hkey)));
        }
        if !write {
            return Ok(None);
        }

        // SAFETY: Creating a non-volatile key under `hroot`.
        let status = unsafe {
            RegCreateKeyExW(
                hroot,
                PCWSTR(wide.as_ptr()),
                Some(0),
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_QUERY_VALUE | KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(Some(OwnedRegKey(hkey)))
        } else {
            Err(PlatformError::Registry(format!(
                "RegCreateKeyExW('{full}') failed with error {}",
                status.0
            )))
        }
    }

    pub(crate) fn read_reg_sz(key: HKEY, name: Option<&str>) -> Option<String> {
        let name_w = name.map(to_wide);
        let name_ptr = name_w
            .as_ref()
            .map(|v| PCWSTR(v.as_ptr()))
            .unwrap_or(PCWSTR::null());

        let mut val_type = REG_VALUE_TYPE::default();
        let mut byte_len: u32 = 0;
        // SAFETY: First call queries required buffer size.
        let status = unsafe {
            RegQueryValueExW(
                key,
                name_ptr,
                None,
                Some(&mut val_type),
                None,
                Some(&mut byte_len),
            )
        };
        if status != ERROR_SUCCESS || val_type != REG_SZ {
            return None;
        }
        if byte_len == 0 {
            return Some(String::new());
        }
        let mut buf = vec![0u8; byte_len as usize];
        // SAFETY: Buffer has `byte_len` bytes allocated.
        let status = unsafe {
            RegQueryValueExW(
                key,
                name_ptr,
                None,
                Some(&mut val_type),
                Some(buf.as_mut_ptr()),
                Some(&mut byte_len),
            )
        };
        if status != ERROR_SUCCESS || val_type != REG_SZ {
            return None;
        }
        let u16s: Vec<u16> = buf[..byte_len as usize]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let end = u16s.iter().position(|&c| c == 0).unwrap_or(u16s.len());
        Some(String::from_utf16_lossy(&u16s[..end]))
    }

    pub(crate) fn read_reg_dword(root: &RegistryRoot, subkey: &str, name: &str) -> Option<u32> {
        let key = open_reg_key(root, subkey, false).ok()??;
        let name_w = to_wide(name);
        let mut val_type = REG_VALUE_TYPE::default();
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: Reading a 4-byte DWORD value into `value`.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut val_type),
                Some((&mut value as *mut u32).cast::<u8>()),
                Some(&mut size),
            )
        };
        if status == ERROR_SUCCESS && val_type == REG_DWORD && size == 4 {
            Some(value)
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn write_reg_dword(
        root: &RegistryRoot,
        subkey: &str,
        name: &str,
        value: u32,
    ) -> Result<()> {
        let key = open_reg_key(root, subkey, true)?
            .ok_or_else(|| PlatformError::Registry(format!("could not open '{subkey}'")))?;
        let name_w = to_wide(name);
        let bytes = value.to_le_bytes();
        // SAFETY: Writing a 4-byte REG_DWORD value.
        let status = unsafe {
            RegSetValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                Some(0),
                REG_DWORD,
                Some(&bytes),
            )
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(PlatformError::Registry(format!(
                "RegSetValueExW('{name}') failed with error {}",
                status.0
            )))
        }
    }

    pub(crate) fn set_reg_sz(key: HKEY, name: Option<&str>, value: &str) -> Result<()> {
        let name_w = name.map(to_wide);
        let name_ptr = name_w
            .as_ref()
            .map(|v| PCWSTR(v.as_ptr()))
            .unwrap_or(PCWSTR::null());
        let val_w = to_wide(value);
        // SAFETY: `val_w` is null-terminated UTF-16; byte length is `val_w.len() * 2`.
        let bytes =
            unsafe { std::slice::from_raw_parts(val_w.as_ptr().cast::<u8>(), val_w.len() * 2) };
        // SAFETY: Valid HKEY, name pointer, and REG_SZ byte slice.
        let status = unsafe { RegSetValueExW(key, name_ptr, Some(0), REG_SZ, Some(bytes)) };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(PlatformError::Registry(format!(
                "RegSetValueExW failed with error {}",
                status.0
            )))
        }
    }

    fn check_or_write_sz(key: HKEY, name: Option<&str>, value: &str, write: bool) -> Result<bool> {
        if read_reg_sz(key, name).as_deref() == Some(value) {
            return Ok(true);
        }
        if !write {
            return Ok(false);
        }
        set_reg_sz(key, name, value)?;
        Ok(true)
    }

    pub(crate) fn remove_reg_key(root: &RegistryRoot, subkey: &str) -> bool {
        let full = root.resolve(subkey);
        let wide = to_wide(&full);
        // SAFETY: Deleting an empty registry key.
        unsafe { RegDeleteKeyW(root_hkey(root.hive), PCWSTR(wide.as_ptr())) == ERROR_SUCCESS }
    }

    pub(crate) fn remove_reg_value(key: HKEY, name: &str) -> bool {
        let wide = to_wide(name);
        // SAFETY: Deleting a named value from an opened key.
        unsafe { RegDeleteValueW(key, PCWSTR(wide.as_ptr())) == ERROR_SUCCESS }
    }

    pub(crate) fn delete_tree(hive: RegistryHive, subkey: &str) -> Result<()> {
        let wide = to_wide(subkey);
        // SAFETY: Caller verified `subkey` is non-empty.
        let status = unsafe { RegDeleteTreeW(root_hkey(hive), PCWSTR(wide.as_ptr())) };
        if status == ERROR_SUCCESS || status.0 == 2 {
            // ERROR_FILE_NOT_FOUND (2) is fine for idempotent cleanup.
            Ok(())
        } else {
            Err(PlatformError::Registry(format!(
                "RegDeleteTreeW('{subkey}') failed with error {}",
                status.0
            )))
        }
    }

    fn register_legacy(root: &RegistryRoot, d: &UrlSchemeDescriptor, write: bool) -> Result<bool> {
        let key_base = format!("Software\\Classes\\{}", d.protocol);
        let command = d.open_command();
        let default_icon = d.default_icon_value();
        let url_desc = format!("URL:{}", d.protocol_name);

        let Some(k_base) = open_reg_key(root, &key_base, write)? else {
            return Ok(false);
        };
        if !check_or_write_sz(k_base.0, Some("URL Protocol"), "", write)? {
            return Ok(false);
        }
        if !check_or_write_sz(k_base.0, None, &url_desc, write)? {
            return Ok(false);
        }

        let Some(k_icon) = open_reg_key(root, &format!("{key_base}\\DefaultIcon"), write)? else {
            return Ok(false);
        };
        if !check_or_write_sz(k_icon.0, None, &default_icon, write)? {
            return Ok(false);
        }

        let Some(_k_shell) = open_reg_key(root, &format!("{key_base}\\shell"), write)? else {
            return Ok(false);
        };
        let Some(_k_open) = open_reg_key(root, &format!("{key_base}\\shell\\open"), write)? else {
            return Ok(false);
        };
        let Some(k_cmd) = open_reg_key(root, &format!("{key_base}\\shell\\open\\command"), write)?
        else {
            return Ok(false);
        };
        check_or_write_sz(k_cmd.0, None, &command, write)
    }

    pub(crate) fn unregister_legacy(root: &RegistryRoot, d: &UrlSchemeDescriptor) {
        let key_base = format!("Software\\Classes\\{}", d.protocol);
        remove_reg_key(root, &format!("{key_base}\\shell\\open\\command"));
        remove_reg_key(root, &format!("{key_base}\\shell\\open"));
        remove_reg_key(root, &format!("{key_base}\\shell"));
        remove_reg_key(root, &format!("{key_base}\\DefaultIcon"));
        remove_reg_key(root, &key_base);
    }

    fn register_default_program(
        root: &RegistryRoot,
        d: &UrlSchemeDescriptor,
        write: bool,
    ) -> Result<bool> {
        let named_protocol = d.named_protocol();
        let named_base = format!("Software\\Classes\\{named_protocol}");
        let long_named_base = format!("Software\\{}", d.long_app_name);
        let registered = format!("SOFTWARE\\{}\\Capabilities", d.long_app_name);
        let command = d.open_command();
        let default_icon = d.default_icon_value();

        let Some(_k_named) = open_reg_key(root, &named_base, write)? else {
            return Ok(false);
        };
        let Some(k_icon) = open_reg_key(root, &format!("{named_base}\\DefaultIcon"), write)? else {
            return Ok(false);
        };
        if !check_or_write_sz(k_icon.0, None, &default_icon, write)? {
            return Ok(false);
        }

        let Some(_k_shell) = open_reg_key(root, &format!("{named_base}\\shell"), write)? else {
            return Ok(false);
        };
        let Some(_k_open) = open_reg_key(root, &format!("{named_base}\\shell\\open"), write)?
        else {
            return Ok(false);
        };
        let Some(k_cmd) =
            open_reg_key(root, &format!("{named_base}\\shell\\open\\command"), write)?
        else {
            return Ok(false);
        };
        if !check_or_write_sz(k_cmd.0, None, &command, write)? {
            return Ok(false);
        }

        let Some(_k_long) = open_reg_key(root, &long_named_base, write)? else {
            return Ok(false);
        };
        let Some(k_caps) = open_reg_key(root, &format!("{long_named_base}\\Capabilities"), write)?
        else {
            return Ok(false);
        };
        if !check_or_write_sz(
            k_caps.0,
            Some("ApplicationName"),
            &d.display_app_name,
            write,
        )? {
            return Ok(false);
        }
        if !check_or_write_sz(
            k_caps.0,
            Some("ApplicationDescription"),
            &d.display_app_description,
            write,
        )? {
            return Ok(false);
        }

        let Some(k_assoc) = open_reg_key(
            root,
            &format!("{long_named_base}\\Capabilities\\UrlAssociations"),
            write,
        )?
        else {
            return Ok(false);
        };
        if !check_or_write_sz(k_assoc.0, Some(&d.protocol), &named_protocol, write)? {
            return Ok(false);
        }

        let Some(k_reg_apps) = open_reg_key(root, "Software\\RegisteredApplications", write)?
        else {
            return Ok(false);
        };
        check_or_write_sz(k_reg_apps.0, Some(&d.display_app_name), &registered, write)
    }

    fn is_reg_key_empty(key: HKEY) -> bool {
        let mut subkeys: u32 = 0;
        let mut values: u32 = 0;
        // SAFETY: Querying subkey and value counts on a valid opened registry key.
        let status = unsafe {
            RegQueryInfoKeyW(
                key,
                None,
                None,
                None,
                Some(&mut subkeys),
                None,
                None,
                Some(&mut values),
                None,
                None,
                None,
                None,
            )
        };
        status == ERROR_SUCCESS && subkeys == 0 && values == 0
    }

    pub(crate) fn unregister_default_program(root: &RegistryRoot, d: &UrlSchemeDescriptor) {
        let named_protocol = d.named_protocol();
        let named_base = format!("Software\\Classes\\{named_protocol}");
        let long_named_base = format!("Software\\{}", d.long_app_name);
        let assoc_key_path = format!("{long_named_base}\\Capabilities\\UrlAssociations");

        let assoc_empty = if let Ok(Some(k_assoc)) = open_reg_key(root, &assoc_key_path, true) {
            remove_reg_value(k_assoc.0, &d.protocol);
            is_reg_key_empty(k_assoc.0)
        } else {
            true
        };

        // Only remove Capabilities / RegisteredApplications when no other URL associations
        // remain for this application (e.g. both `tg` and `tonsite` share Capabilities).
        if assoc_empty {
            remove_reg_key(root, &assoc_key_path);
            if let Ok(Some(k_reg_apps)) =
                open_reg_key(root, "Software\\RegisteredApplications", true)
            {
                remove_reg_value(k_reg_apps.0, &d.display_app_name);
            }
            remove_reg_key(root, &format!("{long_named_base}\\Capabilities"));
            remove_reg_key(root, &long_named_base);
        }
        remove_reg_key(root, &format!("{named_base}\\shell\\open\\command"));
        remove_reg_key(root, &format!("{named_base}\\shell\\open"));
        remove_reg_key(root, &format!("{named_base}\\shell"));
        remove_reg_key(root, &format!("{named_base}\\DefaultIcon"));
        remove_reg_key(root, &named_base);
    }

    pub(crate) fn full_register(
        root: &RegistryRoot,
        d: &UrlSchemeDescriptor,
        write: bool,
    ) -> Result<bool> {
        Ok(register_default_program(root, d, write)? && register_legacy(root, d, write)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_formats_match_tdesktop() {
        let tg = UrlSchemeDescriptor::tg(
            PathBuf::from("C:/Program Files/Rustgram/rustgram.exe"),
            None,
        );
        assert_eq!(tg.protocol, "tg");
        assert_eq!(tg.named_protocol(), "rustgram.tg");
        assert_eq!(
            tg.open_command(),
            "\"C:\\Program Files\\Rustgram\\rustgram.exe\"  -- \"%1\""
        );
        assert_eq!(
            tg.default_icon_value(),
            "\"C:\\Program Files\\Rustgram\\rustgram.exe,1\""
        );

        let tonsite = UrlSchemeDescriptor::tonsite(
            PathBuf::from("C:\\Rustgram\\rustgram.exe"),
            Some(Path::new("D:\\data")),
        );
        assert_eq!(tonsite.protocol, "tonsite");
        assert_eq!(tonsite.named_protocol(), "rustgram.tonsite");
        assert_eq!(
            tonsite.open_command(),
            "\"C:\\Rustgram\\rustgram.exe\" -workdir \"D:\\data\" -- \"%1\""
        );
    }

    #[test]
    fn registry_root_refuses_empty_delete() {
        assert!(RegistryRoot::hkcu().delete_tree().is_err());
        assert_eq!(
            RegistryRoot::hkcu_subkey("Software\\RustgramTest\\1").resolve("Software\\Classes\\tg"),
            "Software\\RustgramTest\\1\\Software\\Classes\\tg"
        );
    }

    #[cfg(windows)]
    #[test]
    fn register_check_and_unregister_schemes_in_throwaway_key() {
        let root = RegistryRoot::hkcu_subkey(format!(
            "Software\\RustgramTest\\url_scheme_{}",
            std::process::id()
        ));
        let _ = root.delete_tree();

        let exe = PathBuf::from("C:\\Rustgram\\rustgram.exe");
        let tg = UrlSchemeDescriptor::tg(exe.clone(), None);
        let tonsite = UrlSchemeDescriptor::tonsite(exe.clone(), None);

        assert!(!check_url_scheme(&root, &tg).unwrap());
        assert!(!check_url_scheme(&root, &tonsite).unwrap());

        register_default_schemes(&root, &exe, None).unwrap();
        assert!(check_url_scheme(&root, &tg).unwrap());
        assert!(check_url_scheme(&root, &tonsite).unwrap());

        // Different exe must not match check or unregister.
        let other = UrlSchemeDescriptor::tg(PathBuf::from("C:\\Other\\other.exe"), None);
        assert!(!check_url_scheme(&root, &other).unwrap());
        assert!(!unregister_url_scheme(&root, &other).unwrap());
        assert!(check_url_scheme(&root, &tg).unwrap());

        // Unregister tg and tonsite cleanly.
        assert!(unregister_url_scheme(&root, &tg).unwrap());
        assert!(!check_url_scheme(&root, &tg).unwrap());
        assert!(unregister_url_scheme(&root, &tonsite).unwrap());
        assert!(!check_url_scheme(&root, &tonsite).unwrap());

        root.delete_tree().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn autostart_and_send_to_in_temp_dir() {
        let base = std::env::temp_dir().join(format!("rustgram_links_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let startup = base.join("Startup");
        let send_to = base.join("SendTo");
        std::fs::create_dir_all(&startup).unwrap();
        std::fs::create_dir_all(&send_to).unwrap();

        let exe = crate::app_id::current_executable_path().unwrap();
        let cfg = AppLinkConfig::for_executable(exe, base.clone());

        assert!(!is_autostart_enabled_in(&startup, &cfg.shortcut_name));
        set_autostart_in(&startup, &cfg, true).unwrap();
        assert!(is_autostart_enabled_in(&startup, &cfg.shortcut_name));
        set_autostart_in(&startup, &cfg, false).unwrap();
        assert!(!is_autostart_enabled_in(&startup, &cfg.shortcut_name));

        assert!(!is_send_to_enabled_in(&send_to, &cfg.shortcut_name));
        set_send_to_in(&send_to, &cfg, true).unwrap();
        assert!(is_send_to_enabled_in(&send_to, &cfg.shortcut_name));
        set_send_to_in(&send_to, &cfg, false).unwrap();
        assert!(!is_send_to_enabled_in(&send_to, &cfg.shortcut_name));

        let _ = std::fs::remove_dir_all(&base);
    }
}
