; Rustgram Windows Installer Script (Inno Setup 6)
; Ported from Telegram Desktop's Telegram/build/setup.iss (GPL-3.0)
;
; Invocation from tg-xtask:
;   ISCC.exe /DMyAppVersion=0.1.0 /DMyAppNumericVersion=0.1.0.0 \
;            /DReleasePath=.\target\release \
;            /DSetupIcon=.\dist\rustgram.ico \
;            /DOutputDir=.\dist \
;            /DMyOutputBaseFilename=Rustgram-Setup-0.1.0-win-x64 \
;            packaging\windows\tgfork.iss

#define MyAppShortName "Rustgram"
#define MyAppName "Rustgram"
#define MyAppPublisher "The Rustgram Authors"
#define MyAppURL "https://github.com/rustgram/rustgram"
#define MyAppExeName "rustgram.exe"
#define MyUpdaterExeName "rustgram-updater.exe"
#define MyWidgetHelperExeName "rustgram-widget-helper.exe"
#define MyAppUserModelId "Rustgram"
#define MyAppProgId "rustgram.tg"
; Unique AppId GUID generated for Rustgram (also referenced by tg-updater registry update)
#define MyAppId "46C2F7F5-7D28-455F-8E02-5102E303855E"
#define CurrentYear GetDateTimeString('yyyy','','')

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#ifndef MyAppNumericVersion
  #define MyAppNumericVersion "0.1.0.0"
#endif

#ifndef ReleasePath
  #define ReleasePath "..\..\target\release"
#endif

#ifndef OutputDir
  #define OutputDir "..\..\target\dist"
#endif

#ifndef SetupIcon
  #define SetupIcon OutputDir + "\rustgram.ico"
#endif

#ifndef MyOutputBaseFilename
  #define MyOutputBaseFilename "Rustgram-Setup-" + MyAppVersion + "-win-x64"
#endif

[Setup]
AppId={{{#MyAppId}}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion} (64-bit)
AppCopyright={#MyAppPublisher} {#CurrentYear}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={userappdata}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir={#OutputDir}
OutputBaseFilename={#MyOutputBaseFilename}
SetupIconFile={#SetupIcon}
UninstallDisplayName={#MyAppName}
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma
SolidCompression=yes
DisableStartupPrompt=yes
PrivilegesRequired=lowest
VersionInfoVersion={#MyAppNumericVersion}
CloseApplications=force
DisableDirPage=no
DisableProgramGroupPage=no
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
#ifdef MySignTool
SignTool={#MySignTool}
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
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#ReleasePath}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ReleasePath}\{#MyUpdaterExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ReleasePath}\{#MyWidgetHelperExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppShortName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppUserModelId}"
Name: "{group}\{cm:UninstallProgram,{#MyAppShortName}}"; Filename: "{uninstallexe}"
Name: "{userdesktop}\{#MyAppShortName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppUserModelId}"; Tasks: desktopicon

[Registry]
; Per-user tg:// URL protocol registration matching base_url_scheme_win.cpp / tg-platform.
; Both the legacy HKCU\Software\Classes\tg handler and Windows Default Programs
; Capabilities\UrlAssociations are registered and cleanly removed on uninstall.

; Legacy tg:// protocol key
Root: HKCU; Subkey: "Software\Classes\tg"; ValueType: string; ValueName: ""; ValueData: "URL:Rustgram Link"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\tg"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\tg\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"",0"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\tg\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" -- ""%1"""; Flags: uninsdeletekey

; Default Programs ProgID (rustgram.tg)
Root: HKCU; Subkey: "Software\Classes\{#MyAppProgId}"; ValueType: string; ValueName: ""; ValueData: "URL:Rustgram Link"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\{#MyAppProgId}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"",0"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\{#MyAppProgId}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" -- ""%1"""; Flags: uninsdeletekey

; Windows Default Programs Capabilities & RegisteredApplications
Root: HKCU; Subkey: "Software\{#MyAppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\{#MyAppName}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#MyAppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\{#MyAppName}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Rustgram Desktop Client"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\{#MyAppName}\Capabilities\UrlAssociations"; ValueType: string; ValueName: "tg"; ValueData: "{#MyAppProgId}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: "SOFTWARE\{#MyAppName}\Capabilities"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppShortName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{app}\data"
Type: files; Name: "{app}\data_config"
Type: files; Name: "{app}\log.txt"
Type: files; Name: "{app}\log_start0.txt"
Type: files; Name: "{app}\log_start1.txt"
Type: files; Name: "{app}\log_start2.txt"
Type: files; Name: "{app}\log_start3.txt"
Type: files; Name: "{app}\log_start4.txt"
Type: files; Name: "{app}\log_start5.txt"
Type: files; Name: "{app}\log_start6.txt"
Type: files; Name: "{app}\log_start7.txt"
Type: files; Name: "{app}\log_start8.txt"
Type: files; Name: "{app}\log_start9.txt"
Type: filesandordirs; Name: "{app}\DebugLogs"
Type: filesandordirs; Name: "{app}\tupdates"
Type: filesandordirs; Name: "{app}\tdata"
Type: filesandordirs; Name: "{app}\tcache"
Type: filesandordirs; Name: "{app}\tdumps"
Type: dirifempty; Name: "{app}"
Type: files; Name: "{userappdata}\{#MyAppName}\data"
Type: files; Name: "{userappdata}\{#MyAppName}\data_config"
Type: files; Name: "{userappdata}\{#MyAppName}\log.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start0.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start1.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start2.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start3.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start4.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start5.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start6.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start7.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start8.txt"
Type: files; Name: "{userappdata}\{#MyAppName}\log_start9.txt"
Type: filesandordirs; Name: "{userappdata}\{#MyAppName}\DebugLogs"
Type: filesandordirs; Name: "{userappdata}\{#MyAppName}\tupdates"
Type: filesandordirs; Name: "{userappdata}\{#MyAppName}\tdata"
Type: filesandordirs; Name: "{userappdata}\{#MyAppName}\tcache"
Type: filesandordirs; Name: "{userappdata}\{#MyAppName}\tdumps"
Type: dirifempty; Name: "{userappdata}\{#MyAppName}"

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    Exec(ExpandConstant('{app}\{#MyAppExeName}'), '-cleanup', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;
end;
