; Rustgram Windows Installer Script (Inno Setup 6)
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
#define MyAppURL "https://github.com/murmaider227/rustgram"
#define MyAppExeName "rustgram.exe"
#define MyUpdaterExeName "rustgram-updater.exe"
#define MyWidgetHelperExeName "rustgram-widget-helper.exe"
#define MyAppUserModelId "Rustgram"
#define MyAppProgId "rustgram.tg"
; Unique AppId GUID shared with tg-updater uninstall registry updates
#define MyAppId "46C2F7F5-7D28-455F-8E02-5102E303855E"
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
InfoBeforeFile={#DistDir}\PRIVACY.txt
VersionInfoVersion={#MyAppNumericVersion}
VersionInfoTextVersion={#MyAppVersion}
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppNumericVersion}
VersionInfoProductTextVersion={#MyAppVersion}
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription={#MyAppName} Setup
VersionInfoCopyright=Copyright (c) {#CurrentYear} {#MyAppPublisher}
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
Name: "autoupdate"; Description: "Check for updates automatically"; GroupDescription: "Updates:"; Flags: checkedonce
Name: "registertg"; Description: "Register tg:// protocol handler"; GroupDescription: "Protocol associations:"; Flags: unchecked

[Files]
Source: "{#DistDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\{#MyUpdaterExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\{#MyWidgetHelperExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\avcodec-*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\avformat-*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\avutil-*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\swresample-*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\swscale-*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\LEGAL"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\THIRD_PARTY_NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\PRIVACY.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppShortName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppUserModelId}"
Name: "{group}\{cm:UninstallProgram,{#MyAppShortName}}"; Filename: "{uninstallexe}"
Name: "{userdesktop}\{#MyAppShortName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppUserModelId}"; Tasks: desktopicon

[Registry]
; Optional per-user tg:// URL protocol registration (controlled by the `registertg` task).
; Legacy HKCU\Software\Classes\tg protocol handler
Root: HKCU; Subkey: "Software\Classes\tg"; ValueType: string; ValueName: ""; ValueData: "URL:Rustgram Link"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\tg"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\tg\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"",0"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\tg\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" -- ""%1"""; Flags: uninsdeletekey; Tasks: registertg

; Default Programs ProgID (rustgram.tg)
Root: HKCU; Subkey: "Software\Classes\{#MyAppProgId}"; ValueType: string; ValueName: ""; ValueData: "URL:Rustgram Link"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\{#MyAppProgId}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"",0"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\Classes\{#MyAppProgId}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" -- ""%1"""; Flags: uninsdeletekey; Tasks: registertg

; Windows Default Programs Capabilities & RegisteredApplications
Root: HKCU; Subkey: "Software\{#MyAppName}"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\{#MyAppName}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#MyAppName}"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\{#MyAppName}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Rustgram Desktop Client"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\{#MyAppName}\Capabilities\UrlAssociations"; ValueType: string; ValueName: "tg"; ValueData: "{#MyAppProgId}"; Flags: uninsdeletekey; Tasks: registertg
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: "SOFTWARE\{#MyAppName}\Capabilities"; Flags: uninsdeletevalue; Tasks: registertg

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
Type: filesandordirs; Name: "{userappdata}\{#MyAppName}\tcache"
Type: filesandordirs; Name: "{userappdata}\{#MyAppName}\tdumps"
Type: dirifempty; Name: "{userappdata}\{#MyAppName}"

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
  PrefsDir := ExpandConstant('{userappdata}\{#MyAppName}\tdata');
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
    else if Trim(Text) = '{}' then
    begin
      RawContent := '{' + #13#10 + '  "auto_update": ' + DesiredVal + #13#10 + '}' + #13#10;
      SaveStringToFile(PrefsFile, RawContent, False);
    end;
  end
  else if ForceDirectories(PrefsDir) then
  begin
    RawContent := '{' + #13#10 + '  "auto_update": ' + DesiredVal + #13#10 + '}' + #13#10;
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
    Exec(ExpandConstant('{app}\{#MyAppExeName}'), '-cleanup', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;
end;
