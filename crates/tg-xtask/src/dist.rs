//! Release distribution pipeline (`cargo run -p tg-xtask -- dist`).
//!
//! Builds `tg-app`, `tg-updater`, and `tg-platform` (`rustgram-widget-helper`) in release mode,
//! renders `assets/icon.svg` to a multi-size `.ico`, stages a ready portable folder
//! (`<out_dir>/Rustgram/`) with `rustgram.exe`, `rustgram-updater.exe`, `rustgram-widget-helper.exe`,
//! required FFmpeg DLLs, and licenses (GPL-3.0 + third-party notices), zips the portable
//! distribution, optionally signs a `.tupdate` package with Ed25519, invokes Inno Setup
//! (`ISCC.exe`) if installed (skipping gracefully otherwise), and writes `SHA256SUMS.txt`.

use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use tg_updater::package::pack_update;
use tg_updater::version::{Target, UpdateChannel, Version};
use tg_updater::{APP_ID_GUID, DEFAULT_EXE_NAME, UPDATER_EXE_NAME};

use crate::XtaskError;
use crate::icon::render_svg_to_ico;

/// Name of the ready-to-run portable distribution folder under `<out_dir>`.
pub const PORTABLE_DIR_NAME: &str = "Rustgram";

/// Filename of the sandboxed widget helper executable shipped alongside `rustgram.exe` and
/// `rustgram-updater.exe`.
pub const WIDGET_HELPER_EXE_NAME: &str = "rustgram-widget-helper.exe";

/// All Windows executables bundled in the Rustgram portable distribution, release zip,
/// signed `.tupdate` archive, and Inno Setup installer.
pub const DIST_EXE_NAMES: &[&str] = &[DEFAULT_EXE_NAME, UPDATER_EXE_NAME, WIDGET_HELPER_EXE_NAME];

/// Filename of the generated third-party license notices bundled in distributions.
pub const THIRD_PARTY_NOTICES_NAME: &str = "THIRD_PARTY_NOTICES.txt";

/// Filename of the plain-text privacy policy shipped alongside `rustgram.exe` and shown in the installer.
pub const PRIVACY_NOTICE_NAME: &str = "PRIVACY.txt";

/// FFmpeg shared library prefixes linked by `crates/tg-video/build.rs`.
pub const REQUIRED_FFMPEG_DLL_PREFIXES: &[&str] = &[
    "avcodec-",
    "avformat-",
    "avutil-",
    "swresample-",
    "swscale-",
];

/// Configuration for [`run_dist`].
#[derive(Debug, Clone)]
pub struct DistOptions {
    /// Workspace root directory.
    pub workspace_root: PathBuf,
    /// Release semantic version (e.g. `"0.1.0"` or `"0.2.0-beta.1"`).
    pub version: Version,
    /// Release channel (`Stable` or `Beta`).
    pub channel: UpdateChannel,
    /// Output directory where `Rustgram/`, `.ico`, portable ZIP, installer, `.tupdate`, and `SHA256SUMS.txt` are written.
    pub out_dir: PathBuf,
    /// Optional explicit directory containing prebuilt release executables.
    pub release_dir: Option<PathBuf>,
    /// Optional explicit directory containing FFmpeg runtime DLLs (`avcodec-*.dll`, etc.).
    pub ffmpeg_bin_dir: Option<PathBuf>,
    /// Skip running `cargo build --release`.
    pub skip_build: bool,
    /// Optional path to a hex-encoded or raw 32-byte Ed25519 signing seed file.
    pub signing_key_path: Option<PathBuf>,
    /// Optional explicit path to `ISCC.exe`.
    pub iscc_path: Option<PathBuf>,
    /// Fail with an error instead of skipping if `ISCC.exe` is not installed.
    pub require_iscc: bool,
    /// Fail unless `RUSTGRAM_API_ID` / `RUSTGRAM_API_HASH` are set for the build (release CI).
    pub require_api_credentials: bool,
}

/// Artifacts produced by [`run_dist`].
#[derive(Debug, Clone)]
pub struct DistOutput {
    /// Path to generated multi-size `rustgram.ico`.
    pub icon_path: PathBuf,
    /// Path to the staged portable folder (`<out_dir>/Rustgram`).
    pub portable_dir: PathBuf,
    /// Path to generated portable ZIP archive.
    pub portable_zip: PathBuf,
    /// Path to generated Inno Setup installer, if `ISCC.exe` was present.
    pub installer_exe: Option<PathBuf>,
    /// Path to Ed25519-signed `.tupdate` package, if a signing key was configured.
    pub update_package: Option<PathBuf>,
    /// Path to `SHA256SUMS.txt`.
    pub checksums_path: PathBuf,
}

/// Locates the Rustgram workspace root by walking upward from the current directory and
/// `CARGO_MANIFEST_DIR`.
pub fn find_workspace_root() -> Result<PathBuf, XtaskError> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd);
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    for start in candidates {
        for ancestor in start.ancestors() {
            if ancestor.join("Cargo.toml").is_file()
                && ancestor.join("assets").join("icon.svg").is_file()
            {
                return Ok(ancestor.to_path_buf());
            }
        }
    }
    Err(XtaskError::WorkspaceNotFound)
}

/// Reads the workspace package version from root `Cargo.toml` (falling back to `"0.1.0"`).
pub fn read_workspace_version(workspace_root: &Path) -> Result<Version, XtaskError> {
    let cargo_toml = fs::read_to_string(workspace_root.join("Cargo.toml"))?;
    let mut in_workspace_pkg = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace_pkg = trimmed == "[workspace.package]";
            continue;
        }
        if in_workspace_pkg
            && let Some(rest) = trimmed.strip_prefix("version")
            && let Some((_, val)) = rest.split_once('=')
        {
            let clean = val.trim().trim_matches('"');
            return Ok(Version::parse(clean)?);
        }
    }
    Ok(Version::new(0, 1, 0))
}

/// Resolves the Cargo target directory (`CARGO_TARGET_DIR` if set, otherwise `<workspace_root>/target`).
#[must_use]
pub fn resolve_target_dir(workspace_root: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"))
}

/// Locates the directory containing FFmpeg runtime DLLs from:
/// 1. Explicit `override_dir` (`--ffmpeg-bin`)
/// 2. `FFMPEG_DIR/bin` environment variable
/// 3. Any directory on `PATH` containing `avcodec-*.dll`
#[must_use]
pub fn resolve_ffmpeg_bin_dir(override_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        return dir.is_dir().then(|| dir.to_path_buf());
    }
    if let Some(ffmpeg_dir) = env::var_os("FFMPEG_DIR").filter(|v| !v.is_empty()) {
        let candidate = PathBuf::from(ffmpeg_dir).join("bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            if dir.is_dir()
                && fs::read_dir(&dir).ok().is_some_and(|mut entries| {
                    entries.any(|e| {
                        e.ok().is_some_and(|entry| {
                            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                            name.starts_with("avcodec-") && name.ends_with(".dll")
                        })
                    })
                })
            {
                return Some(dir);
            }
        }
    }
    None
}

/// Collects the required FFmpeg runtime DLLs (`avcodec`, `avformat`, `avutil`, `swresample`,
/// `swscale`) from `ffmpeg_bin_dir`, returning sorted `(filename, path)` pairs.
pub fn collect_required_ffmpeg_dlls(
    ffmpeg_bin_dir: &Path,
) -> Result<Vec<(String, PathBuf)>, XtaskError> {
    if !ffmpeg_bin_dir.is_dir() {
        return Err(XtaskError::MissingArtifact(format!(
            "FFmpeg bin directory '{}' not found",
            ffmpeg_bin_dir.display()
        )));
    }

    let mut entries: Vec<(String, PathBuf)> = fs::read_dir(ffmpeg_bin_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = entry.file_name().to_str()?.to_owned();
            Some((name, path))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut selected = Vec::with_capacity(REQUIRED_FFMPEG_DLL_PREFIXES.len());
    for &prefix in REQUIRED_FFMPEG_DLL_PREFIXES {
        let matched = entries.iter().find(|(name, _)| {
            let lower = name.to_ascii_lowercase();
            lower.starts_with(prefix) && lower.ends_with(".dll")
        });
        match matched {
            Some((name, path)) => selected.push((name.clone(), path.clone())),
            None => {
                return Err(XtaskError::MissingArtifact(format!(
                    "required FFmpeg DLL '{prefix}*.dll' not found in '{}'",
                    ffmpeg_bin_dir.display()
                )));
            }
        }
    }
    Ok(selected)
}

/// Generates the `THIRD_PARTY_NOTICES.txt` file bundled alongside `LICENSE` (GPL-3.0) and `LEGAL`.
#[must_use]
pub fn generate_third_party_notices() -> String {
    "\
Rustgram Third-Party Notices and Licenses
=========================================

Rustgram is licensed under the GNU General Public License v3.0 or later
(GPL-3.0-or-later WITH openvpn-openssl-exception). See LICENSE and LEGAL
in this distribution for the full license text.

Major Third-Party Components:

1. tdesktop (upstream C++/Qt design and protocol reference)
   License: GNU General Public License v3.0 with OpenSSL exception
   Source:  https://github.com/telegramdesktop/tdesktop

2. FFmpeg (avcodec, avformat, avutil, swresample, swscale)
   License: GNU General Public License v3.0 (GPL shared build)
   Source:  https://ffmpeg.org/
   Complete corresponding FFmpeg source code is available from the FFmpeg
   project and matching build scripts.

3. Telegram Desktop sounds (Telegram/Resources/sounds, tag v7.4.5)
   Copyright (c) 2014-2026 The Telegram Desktop Authors.
   License: GNU General Public License v3.0 or later with OpenSSL exception
   Source:  https://github.com/telegramdesktop/tdesktop
   Embedded unmodified:
   - msg_incoming.mp3, the default notification sound;
   - call_incoming.mp3, call_outgoing.mp3, call_connect.mp3, call_busy.mp3,
     call_end.mp3, the call sounds;
   - group_call_start.mp3, group_call_connect.mp3, group_call_allowed.mp3,
     group_call_end.mp3, group_call_recording_start.mp3, the group call
     sounds.

4. Rust Crate Dependencies (MIT / Apache-2.0 / BSD / ISC / Zlib / MPL-2.0)
   - egui / eframe / wgpu / winit: MIT OR Apache-2.0
   - tokio / tracing / tracing-subscriber / serde / serde_json: MIT OR Apache-2.0
   - reqwest / rustls / webpki-roots: MIT OR Apache-2.0
   - ed25519-dalek / sha2 / aes / pbkdf2 / md-5: MIT OR Apache-2.0
   - resvg / tiny-skia / image / zip / flate2: MPL-2.0 / BSD-3-Clause / MIT / Apache-2.0
   - rusqlite (bundled SQLite): MIT / Public Domain
   - cpal / opus / ogg: Apache-2.0 / BSD-3-Clause / MIT
   - windows / windows-sys: MIT OR Apache-2.0

Each third-party component remains copyright of its respective authors and
governed by its respective license terms.
"
    .to_owned()
}

/// Generates a valid Inno Setup 6 script (`installer/tgfork.iss`) adapted from tdesktop's
/// `Telegram/build/setup.iss` with zero Telegram branding, product name `"Rustgram"`,
/// FFmpeg DLLs + licenses, and optional `tg://` protocol registration.
#[must_use]
pub fn generate_inno_setup_script() -> String {
    let bare_guid = APP_ID_GUID.trim_matches(|c| c == '{' || c == '}');
    format!(
        r#"; Rustgram Windows Installer Script (Inno Setup 6)
; Adapted from tdesktop's setup.iss (GPL-3.0)
;
; Invocation from tg-xtask:
;   ISCC.exe /DMyAppVersion=0.1.0 /DMyAppNumericVersion=0.1.0.0 \
;            /DDistDir=.\dist\Rustgram \
;            /DSetupIcon=.\dist\rustgram.ico \
;            /DOutputDir=.\dist \
;            /DMyOutputBaseFilename=Rustgram-Setup-0.1.0-win-x64 \
;            installer\tgfork.iss

#define MyAppShortName "Rustgram"
#define MyAppName "Rustgram"
#define MyAppPublisher "Rustgram Authors"
#define MyAppURL "https://github.com/rustgram/rustgram"
#define MyAppExeName "rustgram.exe"
#define MyUpdaterExeName "rustgram-updater.exe"
#define MyWidgetHelperExeName "rustgram-widget-helper.exe"
#define MyAppUserModelId "Rustgram"
#define MyAppProgId "rustgram.tg"
; Unique AppId GUID shared with tg-updater uninstall registry updates
#define MyAppId "{bare_guid}"
#define CurrentYear GetDateTimeString('yyyy','','')

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#ifndef MyAppNumericVersion
  #define MyAppNumericVersion "0.1.0.0"
#endif

#ifndef DistDir
  #define DistDir "..\target\dist\Rustgram"
#endif

#ifndef OutputDir
  #define OutputDir "..\target\dist"
#endif

#ifndef SetupIcon
  #define SetupIcon OutputDir + "\rustgram.ico"
#endif

#ifndef MyOutputBaseFilename
  #define MyOutputBaseFilename "Rustgram-Setup-" + MyAppVersion + "-win-x64"
#endif

[Setup]
AppId={{{{{{#MyAppId}}}}
AppName={{#MyAppName}}
AppVersion={{#MyAppVersion}}
AppVerName={{#MyAppName}} {{#MyAppVersion}} (64-bit)
AppCopyright={{#MyAppPublisher}} {{#CurrentYear}}
AppPublisher={{#MyAppPublisher}}
AppPublisherURL={{#MyAppURL}}
AppSupportURL={{#MyAppURL}}
AppUpdatesURL={{#MyAppURL}}
DefaultDirName={{userappdata}}\{{#MyAppName}}
DefaultGroupName={{#MyAppName}}
AllowNoIcons=yes
OutputDir={{#OutputDir}}
OutputBaseFilename={{#MyOutputBaseFilename}}
SetupIconFile={{#SetupIcon}}
UninstallDisplayName={{#MyAppName}}
UninstallDisplayIcon={{app}}\{{#MyAppExeName}}
Compression=lzma
SolidCompression=yes
DisableStartupPrompt=yes
PrivilegesRequired=lowest
InfoBeforeFile={{#DistDir}}\PRIVACY.txt
VersionInfoVersion={{#MyAppNumericVersion}}
VersionInfoTextVersion={{#MyAppVersion}}
VersionInfoProductName={{#MyAppName}}
VersionInfoProductVersion={{#MyAppNumericVersion}}
VersionInfoProductTextVersion={{#MyAppVersion}}
VersionInfoCompany={{#MyAppPublisher}}
VersionInfoDescription={{#MyAppName}} Setup
VersionInfoCopyright=Copyright (c) {{#CurrentYear}} {{#MyAppPublisher}}
CloseApplications=force
DisableDirPage=no
DisableProgramGroupPage=no
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
#ifdef MySignTool
SignTool={{#MySignTool}}
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "it";      MessagesFile: "compiler:Languages\Italian.isl"
Name: "es";      MessagesFile: "compiler:Languages\Spanish.isl"
Name: "de";      MessagesFile: "compiler:Languages\German.isl"
Name: "nl";      MessagesFile: "compiler:Languages\Dutch.isl"
Name: "pt_BR";   MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "ru";      MessagesFile: "compiler:Languages\Russian.isl"
Name: "fr";      MessagesFile: "compiler:Languages\French.isl"
Name: "ua";      MessagesFile: "compiler:Languages\Ukrainian.isl"

[Tasks]
Name: "desktopicon"; Description: "{{cm:CreateDesktopIcon}}"; GroupDescription: "{{cm:AdditionalIcons}}"
Name: "autoupdate"; Description: "Check for updates automatically"; GroupDescription: "Updates:"; Flags: checkedonce
Name: "registertg"; Description: "Register tg:// protocol handler"; GroupDescription: "Protocol associations:"; Flags: checkedonce

[Files]
Source: "{{#DistDir}}\{{#MyAppExeName}}"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\{{#MyUpdaterExeName}}"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\{{#MyWidgetHelperExeName}}"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\avcodec-*.dll"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\avformat-*.dll"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\avutil-*.dll"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\swresample-*.dll"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\swscale-*.dll"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\LICENSE"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\LEGAL"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\THIRD_PARTY_NOTICES.txt"; DestDir: "{{app}}"; Flags: ignoreversion
Source: "{{#DistDir}}\PRIVACY.txt"; DestDir: "{{app}}"; Flags: ignoreversion

[Icons]
Name: "{{group}}\{{#MyAppShortName}}"; Filename: "{{app}}\{{#MyAppExeName}}"; AppUserModelID: "{{#MyAppUserModelId}}"
Name: "{{group}}\{{cm:UninstallProgram,{{#MyAppShortName}}}}"; Filename: "{{uninstallexe}}"
Name: "{{userdesktop}}\{{#MyAppShortName}}"; Filename: "{{app}}\{{#MyAppExeName}}"; AppUserModelID: "{{#MyAppUserModelId}}"; Tasks: desktopicon

[Registry]
; Optional per-user tg:// URL protocol registration (controlled by the `registertg` task).
; Legacy HKCU\Software\Classes\tg protocol handler
Root: HKCU; Subkey: "Software\Classes\tg"; ValueType: string; ValueName: ""; ValueData: "URL:Rustgram Link"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\tg"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\tg\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{{app}}\{{#MyAppExeName}}"",0"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\tg\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{{app}}\{{#MyAppExeName}}"" -- ""%1"""; Flags: uninsdeletekey; Tasks: registertg

; Default Programs ProgID (rustgram.tg)
Root: HKCU; Subkey: "Software\Classes\{{#MyAppProgId}}"; ValueType: string; ValueName: ""; ValueData: "URL:Rustgram Link"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\{{#MyAppProgId}}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{{app}}\{{#MyAppExeName}}"",0"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\{{#MyAppProgId}}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{{app}}\{{#MyAppExeName}}"" -- ""%1"""; Flags: uninsdeletekey; Tasks: registertg

; Windows Default Programs Capabilities & RegisteredApplications
Root: HKCU; Subkey: "Software\{{#MyAppName}}"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\{{#MyAppName}}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{{#MyAppName}}"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\{{#MyAppName}}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Rustgram Desktop Client"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\{{#MyAppName}}\Capabilities\UrlAssociations"; ValueType: string; ValueName: "tg"; ValueData: "{{#MyAppProgId}}"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{{#MyAppName}}"; ValueData: "SOFTWARE\{{#MyAppName}}\Capabilities"; Flags: uninsdeletevalue; Tasks: registertg

[Run]
Filename: "{{app}}\{{#MyAppExeName}}"; Description: "{{cm:LaunchProgram,{{#StringChange(MyAppShortName, '&', '&&')}}}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{{app}}\data"
Type: files; Name: "{{app}}\data_config"
Type: files; Name: "{{app}}\log.txt"
Type: files; Name: "{{app}}\log_start0.txt"
Type: files; Name: "{{app}}\log_start1.txt"
Type: files; Name: "{{app}}\log_start2.txt"
Type: files; Name: "{{app}}\log_start3.txt"
Type: files; Name: "{{app}}\log_start4.txt"
Type: files; Name: "{{app}}\log_start5.txt"
Type: files; Name: "{{app}}\log_start6.txt"
Type: files; Name: "{{app}}\log_start7.txt"
Type: files; Name: "{{app}}\log_start8.txt"
Type: files; Name: "{{app}}\log_start9.txt"
Type: filesandordirs; Name: "{{app}}\DebugLogs"
Type: filesandordirs; Name: "{{app}}\tupdates"
Type: filesandordirs; Name: "{{app}}\tcache"
Type: filesandordirs; Name: "{{app}}\tdumps"
Type: dirifempty; Name: "{{app}}"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\data"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\data_config"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start0.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start1.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start2.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start3.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start4.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start5.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start6.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start7.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start8.txt"
Type: files; Name: "{{userappdata}}\{{#MyAppName}}\log_start9.txt"
Type: filesandordirs; Name: "{{userappdata}}\{{#MyAppName}}\DebugLogs"
Type: filesandordirs; Name: "{{userappdata}}\{{#MyAppName}}\tupdates"
Type: filesandordirs; Name: "{{userappdata}}\{{#MyAppName}}\tcache"
Type: filesandordirs; Name: "{{userappdata}}\{{#MyAppName}}\tdumps"
Type: dirifempty; Name: "{{userappdata}}\{{#MyAppName}}"

[Code]
procedure WriteAutoUpdatePref(Enabled: Boolean);
var
  PrefsDir: String;
  PrefsFile: String;
  RawContent: AnsiString;
  Text: String;
  DesiredVal: String;
  OppositeVal: String;
begin
  PrefsDir := ExpandConstant('{{userappdata}}\{{#MyAppName}}\tdata');
  PrefsFile := PrefsDir + '\startup_prefs.json';
  if Enabled then
  begin
    DesiredVal := 'true';
    OppositeVal := 'false';
  end
  else
  begin
    DesiredVal := 'false';
    OppositeVal := 'true';
  end;

  if FileExists(PrefsFile) and LoadStringFromFile(PrefsFile, RawContent) then
  begin
    Text := String(RawContent);
    if Pos('"auto_update"', Text) > 0 then
    begin
      StringChangeEx(Text, '"auto_update": ' + OppositeVal, '"auto_update": ' + DesiredVal, True);
      StringChangeEx(Text, '"auto_update":' + OppositeVal, '"auto_update": ' + DesiredVal, True);
      SaveStringToFile(PrefsFile, AnsiString(Text), False);
    end
    else if Trim(Text) = '{{}}' then
    begin
      RawContent := '{{' + #13#10 + '  "auto_update": ' + DesiredVal + #13#10 + '}}' + #13#10;
      SaveStringToFile(PrefsFile, RawContent, False);
    end;
  end
  else if ForceDirectories(PrefsDir) then
  begin
    RawContent := '{{' + #13#10 + '  "auto_update": ' + DesiredVal + #13#10 + '}}' + #13#10;
    SaveStringToFile(PrefsFile, RawContent, False);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WriteAutoUpdatePref(WizardIsTaskSelected('autoupdate'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    Exec(ExpandConstant('{{app}}\{{#MyAppExeName}}'), '-cleanup', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;
end;
"#
    )
}

/// `/D` defines handed to `ISCC.exe` for the installer script.
#[derive(Debug, Clone)]
pub struct InnoSetupDefines<'a> {
    /// `MyAppVersion` (semver string).
    pub version: &'a str,
    /// `MyAppNumericVersion` (Windows `a.b.c.d` quad).
    pub numeric_version: &'a str,
    /// `DistDir`: staged portable folder.
    pub dist_dir: &'a Path,
    /// `ReleasePath`: cargo release output folder.
    pub release_dir: &'a Path,
    /// `PrivacyFile`: staged privacy notice.
    pub privacy_file: &'a Path,
    /// `SetupIcon`: installer icon.
    pub setup_icon: &'a Path,
    /// `OutputDir`: where ISCC writes the installer.
    pub output_dir: &'a Path,
    /// `MyOutputBaseFilename`: installer file name without `.exe`.
    pub output_base_filename: &'a str,
}

impl InnoSetupDefines<'_> {
    /// Builds the `/D...` arguments. ISCC resolves relative paths against the `.iss` file's folder
    /// (`installer/`), not the working directory, so every path is made absolute first.
    pub fn to_args(&self) -> Result<Vec<String>, XtaskError> {
        let abs = |path: &Path| -> Result<String, XtaskError> {
            Ok(std::path::absolute(path)?.display().to_string())
        };
        Ok(vec![
            format!("/DMyAppVersion={}", self.version),
            format!("/DMyAppNumericVersion={}", self.numeric_version),
            format!("/DDistDir={}", abs(self.dist_dir)?),
            format!("/DReleasePath={}", abs(self.release_dir)?),
            format!("/DPrivacyFile={}", abs(self.privacy_file)?),
            format!("/DSetupIcon={}", abs(self.setup_icon)?),
            format!("/DOutputDir={}", abs(self.output_dir)?),
            format!("/DMyOutputBaseFilename={}", self.output_base_filename),
        ])
    }
}

/// Executes the full distribution workflow.
/// Checks that a build made with `RUSTGRAM_API_ID` / `RUSTGRAM_API_HASH` set really compiled
/// them in (`tg-session` reads them with `option_env!`): `app_bytes` must contain the hash.
/// Returns `Ok(true)` when the check ran, `Ok(false)` when no credentials were set and
/// `required` is `false`. Error messages never include the values.
pub fn check_embedded_api_credentials(
    app_bytes: &[u8],
    env_id: Option<&str>,
    env_hash: Option<&str>,
    required: bool,
) -> Result<bool, XtaskError> {
    let id = env_id.map(str::trim).filter(|v| !v.is_empty());
    let hash = env_hash.map(str::trim).filter(|v| !v.is_empty());
    match (id, hash) {
        (None, None) if !required => Ok(false),
        (Some(_), Some(hash)) => {
            let needle = hash.as_bytes();
            if app_bytes.windows(needle.len()).any(|w| w == needle) {
                Ok(true)
            } else {
                Err(XtaskError::InvalidArgument(
                    "rustgram.exe does not contain the RUSTGRAM_API_HASH set for this build; \
                     the release would start without Telegram API credentials (rebuild without \
                     --skip-build so tg-session compiles them in)"
                        .to_owned(),
                ))
            }
        }
        _ => Err(XtaskError::InvalidArgument(
            "both RUSTGRAM_API_ID and RUSTGRAM_API_HASH must be set for a release build".to_owned(),
        )),
    }
}

pub fn run_dist(opts: &DistOptions) -> Result<DistOutput, XtaskError> {
    // Absolute from here on: child processes (cargo for the icon path, ISCC) resolve relative paths
    // against their own working directories, not ours.
    let out_dir = std::path::absolute(&opts.out_dir)?;
    fs::create_dir_all(&out_dir)?;

    // 1. Render assets/icon.svg -> multi-size rustgram.ico
    let svg_path = opts.workspace_root.join("assets").join("icon.svg");
    let icon_path = out_dir.join("rustgram.ico");
    println!(
        "tg-xtask: rendering {} -> {}",
        svg_path.display(),
        icon_path.display()
    );
    render_svg_to_ico(&svg_path, &icon_path)?;

    let target_dir = resolve_target_dir(&opts.workspace_root);
    let release_dir = std::path::absolute(
        opts.release_dir
            .clone()
            .unwrap_or_else(|| target_dir.join("release")),
    )?;

    // 2. Build release binaries unless --skip-build
    if !opts.skip_build {
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        println!("tg-xtask: building release binaries (tg-app, tg-updater, tg-platform)...");
        let status = Command::new(&cargo)
            .current_dir(&opts.workspace_root)
            .args([
                "build",
                "--release",
                "-p",
                "tg-app",
                "-p",
                "tg-updater",
                "-p",
                "tg-platform",
            ])
            .env("RUSTGRAM_ICON_PATH", &icon_path)
            .env("RUSTGRAM_VERSION", opts.version.to_string())
            .status()?;
        if !status.success() {
            return Err(XtaskError::CommandFailed {
                program: "cargo build --release".into(),
                status: status.to_string(),
            });
        }
    }

    let app_bin_name = platform_exe_name("rustgram");
    let updater_bin_name = platform_exe_name("rustgram-updater");
    let widget_helper_bin_name = platform_exe_name("rustgram-widget-helper");
    let app_exe = release_dir.join(app_bin_name);
    let updater_exe = release_dir.join(updater_bin_name);
    let widget_helper_exe = release_dir.join(widget_helper_bin_name);

    if !app_exe.is_file() {
        return Err(XtaskError::MissingArtifact(app_exe.display().to_string()));
    }
    if !updater_exe.is_file() {
        return Err(XtaskError::MissingArtifact(
            updater_exe.display().to_string(),
        ));
    }
    if !widget_helper_exe.is_file() {
        return Err(XtaskError::MissingArtifact(
            widget_helper_exe.display().to_string(),
        ));
    }

    // With --skip-build the binaries may come from another build, so only a required check runs.
    if !opts.skip_build || opts.require_api_credentials {
        let env_id = env::var("RUSTGRAM_API_ID").ok();
        let env_hash = env::var("RUSTGRAM_API_HASH").ok();
        if check_embedded_api_credentials(
            &fs::read(&app_exe)?,
            env_id.as_deref(),
            env_hash.as_deref(),
            opts.require_api_credentials,
        )? {
            println!("tg-xtask: rustgram.exe carries the API credentials set for this build");
        }
    }

    let ffmpeg_bin_dir =
        resolve_ffmpeg_bin_dir(opts.ffmpeg_bin_dir.as_deref()).ok_or_else(|| {
            XtaskError::MissingArtifact(
                "FFmpeg bin directory not found (checked --ffmpeg-bin, FFMPEG_DIR/bin, and PATH)"
                    .to_owned(),
            )
        })?;
    let ffmpeg_dlls = collect_required_ffmpeg_dlls(&ffmpeg_bin_dir)?;

    let license_src = opts.workspace_root.join("LICENSE");
    let legal_src = opts.workspace_root.join("LEGAL");
    let privacy_src = opts.workspace_root.join("docs").join("PRIVACY.md");
    if !license_src.is_file() {
        return Err(XtaskError::MissingArtifact(
            license_src.display().to_string(),
        ));
    }
    if !privacy_src.is_file() {
        return Err(XtaskError::MissingArtifact(
            privacy_src.display().to_string(),
        ));
    }

    // 3. Stage ready portable folder `<out_dir>/Rustgram/`
    let portable_dir = out_dir.join(PORTABLE_DIR_NAME);
    if portable_dir.exists() {
        fs::remove_dir_all(&portable_dir)?;
    }
    fs::create_dir_all(&portable_dir)?;

    println!(
        "tg-xtask: staging portable directory {}",
        portable_dir.display()
    );
    let staged_app = portable_dir.join(DEFAULT_EXE_NAME);
    let staged_updater = portable_dir.join(UPDATER_EXE_NAME);
    let staged_widget_helper = portable_dir.join(WIDGET_HELPER_EXE_NAME);
    fs::copy(&app_exe, &staged_app)?;
    fs::copy(&updater_exe, &staged_updater)?;
    fs::copy(&widget_helper_exe, &staged_widget_helper)?;

    let mut staged_files: Vec<(String, PathBuf)> = vec![
        (DEFAULT_EXE_NAME.to_owned(), staged_app),
        (UPDATER_EXE_NAME.to_owned(), staged_updater),
        (WIDGET_HELPER_EXE_NAME.to_owned(), staged_widget_helper),
    ];

    for (dll_name, dll_src) in &ffmpeg_dlls {
        let dest = portable_dir.join(dll_name);
        fs::copy(dll_src, &dest)?;
        staged_files.push((dll_name.clone(), dest));
    }

    let staged_license = portable_dir.join("LICENSE");
    fs::copy(&license_src, &staged_license)?;
    staged_files.push(("LICENSE".to_owned(), staged_license));

    if legal_src.is_file() {
        let staged_legal = portable_dir.join("LEGAL");
        fs::copy(&legal_src, &staged_legal)?;
        staged_files.push(("LEGAL".to_owned(), staged_legal));
    }

    let staged_notices = portable_dir.join(THIRD_PARTY_NOTICES_NAME);
    fs::write(&staged_notices, generate_third_party_notices())?;
    staged_files.push((THIRD_PARTY_NOTICES_NAME.to_owned(), staged_notices));

    let privacy_raw = fs::read_to_string(&privacy_src)?;
    let privacy_crlf = privacy_raw.replace("\r\n", "\n").replace('\n', "\r\n");
    let staged_privacy = portable_dir.join(PRIVACY_NOTICE_NAME);
    // Prefix UTF-8 BOM so Inno Setup's InfoBeforeFile wizard page renders Unicode characters cleanly.
    fs::write(&staged_privacy, format!("\u{FEFF}{privacy_crlf}"))?;
    staged_files.push((PRIVACY_NOTICE_NAME.to_owned(), staged_privacy.clone()));

    let suffix = Target::WIN_X64.asset_suffix();
    let version_str = opts.version.to_string();

    // 4. Create portable ZIP archive from the staged portable folder contents
    let portable_zip = out_dir.join(format!("rustgram-{version_str}-{suffix}-portable.zip"));
    println!("tg-xtask: creating portable zip {}", portable_zip.display());
    let zip_entries: Vec<(&str, &Path)> = staged_files
        .iter()
        .map(|(name, path)| (name.as_str(), path.as_path()))
        .collect();
    create_portable_zip(&portable_zip, &zip_entries)?;

    let mut artifacts_to_hash = vec![portable_zip.clone()];

    // 5. Optionally create Ed25519-signed .tupdate package
    let update_package = if let Some(signing_key) =
        load_signing_key(opts.signing_key_path.as_deref())?
    {
        let update_path = out_dir.join(format!("rustgram-{version_str}-{suffix}.tupdate"));
        println!("tg-xtask: packing signed update {}", update_path.display());
        let app_bytes = fs::read(&app_exe)?;
        let updater_bytes = fs::read(&updater_exe)?;
        let widget_helper_bytes = fs::read(&widget_helper_exe)?;
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let packed = pack_update(
            opts.channel,
            Target::WIN_X64,
            &opts.version,
            now_unix,
            [
                (DEFAULT_EXE_NAME, app_bytes.as_slice()),
                (UPDATER_EXE_NAME, updater_bytes.as_slice()),
                (WIDGET_HELPER_EXE_NAME, widget_helper_bytes.as_slice()),
            ],
            &signing_key,
        )?;
        fs::write(&update_path, packed)?;
        artifacts_to_hash.push(update_path.clone());
        Some(update_path)
    } else {
        println!(
            "tg-xtask: RUSTGRAM_UPDATER_SIGNING_KEY not set; skipping signed .tupdate generation"
        );
        None
    };

    // 6. Run Inno Setup compiler (ISCC.exe) if available; skip gracefully if absent
    let installer_exe = match locate_iscc(opts.iscc_path.as_deref()) {
        Some(iscc) => {
            let primary_iss = opts.workspace_root.join("installer").join("tgfork.iss");
            let fallback_iss = opts
                .workspace_root
                .join("packaging")
                .join("windows")
                .join("tgfork.iss");
            let iss_script = if primary_iss.is_file() {
                primary_iss
            } else {
                fallback_iss
            };
            let base_name = format!("Rustgram-Setup-{version_str}-{suffix}");
            println!(
                "tg-xtask: running Inno Setup ({}) on {}",
                iscc.display(),
                iss_script.display()
            );
            let numeric_version = opts.version.to_windows_quad();
            let defines = InnoSetupDefines {
                version: &version_str,
                numeric_version: &numeric_version,
                dist_dir: &portable_dir,
                release_dir: &release_dir,
                privacy_file: &staged_privacy,
                setup_icon: &icon_path,
                output_dir: &out_dir,
                output_base_filename: &base_name,
            };
            let status = Command::new(&iscc)
                .args(defines.to_args()?)
                .arg(&iss_script)
                .status()?;
            if !status.success() {
                return Err(XtaskError::CommandFailed {
                    program: iscc.display().to_string(),
                    status: status.to_string(),
                });
            }
            let exe_path = out_dir.join(format!("{base_name}.exe"));
            artifacts_to_hash.push(exe_path.clone());
            Some(exe_path)
        }
        None if opts.require_iscc => return Err(XtaskError::IsccNotFound),
        None => {
            println!("tg-xtask: ISCC.exe not found; skipping Windows installer build");
            None
        }
    };

    // 7. Write SHA256SUMS.txt
    let checksums_path = out_dir.join("SHA256SUMS.txt");
    write_sha256_sums(&checksums_path, &artifacts_to_hash)?;
    println!("tg-xtask: wrote checksums to {}", checksums_path.display());

    Ok(DistOutput {
        icon_path,
        portable_dir,
        portable_zip,
        installer_exe,
        update_package,
        checksums_path,
    })
}

fn platform_exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// Creates a Deflate-compressed portable ZIP archive from `(archive_name, source_path)` pairs.
pub fn create_portable_zip(out_zip: &Path, entries: &[(&str, &Path)]) -> Result<(), XtaskError> {
    if let Some(parent) = out_zip.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(out_zip)?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o755);

    for &(name, src_path) in entries {
        if !src_path.is_file() {
            continue;
        }
        let data = fs::read(src_path)?;
        writer
            .start_file(name, options)
            .map_err(|e| XtaskError::Zip(e.to_string()))?;
        writer.write_all(&data)?;
    }
    writer
        .finish()
        .map_err(|e| XtaskError::Zip(e.to_string()))?;
    Ok(())
}

/// Writes `SHA256SUMS.txt` in standard `<hex>  <filename>` format.
pub fn write_sha256_sums(out_path: &Path, files: &[PathBuf]) -> Result<(), XtaskError> {
    let mut out = String::new();
    for path in files {
        let data = fs::read(path)?;
        let digest = hex::encode(Sha256::digest(&data));
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact");
        out.push_str(&format!("{digest}  {name}\n"));
    }
    fs::write(out_path, out)?;
    Ok(())
}

/// Locates `ISCC.exe` via explicit override, `PATH`, or standard Inno Setup 6 install directories.
#[must_use]
pub fn locate_iscc(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return p.is_file().then(|| p.to_path_buf());
    }

    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join("ISCC.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for env_key in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(base) = env::var_os(env_key) {
            let candidate = PathBuf::from(&base).join("Inno Setup 6").join("ISCC.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
            let programs_candidate = PathBuf::from(&base)
                .join("Programs")
                .join("Inno Setup 6")
                .join("ISCC.exe");
            if programs_candidate.is_file() {
                return Some(programs_candidate);
            }
        }
    }

    None
}

/// Loads an Ed25519 [`SigningKey`] from `--signing-key <path>`, `RUSTGRAM_UPDATER_SIGNING_KEY`,
/// or the legacy `TGFORK_UPDATER_SIGNING_KEY` fallback.
pub fn load_signing_key(explicit_path: Option<&Path>) -> Result<Option<SigningKey>, XtaskError> {
    if let Some(path) = explicit_path {
        let raw = fs::read(path)?;
        return decode_signing_key_bytes(&raw).map(Some);
    }
    for var in ["RUSTGRAM_UPDATER_SIGNING_KEY", "TGFORK_UPDATER_SIGNING_KEY"] {
        if let Ok(env_val) = env::var(var) {
            let trimmed = env_val.trim();
            if !trimmed.is_empty() {
                return decode_signing_key_bytes(trimmed.as_bytes()).map(Some);
            }
        }
    }
    Ok(None)
}

fn decode_signing_key_bytes(raw: &[u8]) -> Result<SigningKey, XtaskError> {
    if raw.len() == 32 {
        let arr: [u8; 32] = raw
            .try_into()
            .map_err(|_| XtaskError::InvalidSigningKey("expected 32 raw bytes".into()))?;
        return Ok(SigningKey::from_bytes(&arr));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|_| {
            XtaskError::InvalidSigningKey("key must be 32 raw bytes or 64 hex chars".into())
        })?
        .trim();
    let decoded = hex::decode(text)
        .map_err(|e| XtaskError::InvalidSigningKey(format!("invalid hex: {e}")))?;
    let arr: [u8; 32] = decoded
        .try_into()
        .map_err(|_| XtaskError::InvalidSigningKey("expected 64 hex chars (32 bytes)".into()))?;
    Ok(SigningKey::from_bytes(&arr))
}
