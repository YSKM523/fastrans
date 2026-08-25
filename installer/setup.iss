; fastrans Windows installer (Inno Setup 6).
; Per-user install (no admin) so the built-in self-updater can write to the
; install dir. Build: ISCC.exe /DMyAppVersion=x.y.z installer\setup.iss

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

[Setup]
AppId={{B7E5A0F3-4C29-4E8D-9B7A-FA57C2E6D311}
AppName=fastrans
AppVersion={#MyAppVersion}
AppVerName=fastrans v{#MyAppVersion}
AppPublisher=YSKM523
AppPublisherURL=https://github.com/YSKM523/fastrans
AppSupportURL=https://github.com/YSKM523/fastrans/issues
DefaultDirName={localappdata}\Programs\fastrans
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=Output
OutputBaseFilename=fastrans-v{#MyAppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\fastrans.exe
WizardStyle=modern

[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "开机自动启动(后台待命,按热键呼出)"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "..\target\release\fastrans.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\models\opus-mt-zh-en\*"; DestDir: "{app}\models\opus-mt-zh-en"; Flags: ignoreversion recursesubdirs
Source: "..\docs\使用说明.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\fastrans"; Filename: "{app}\fastrans.exe"
Name: "{userdesktop}\fastrans"; Filename: "{app}\fastrans.exe"; Tasks: desktopicon
Name: "{userstartup}\fastrans"; Filename: "{app}\fastrans.exe"; Tasks: autostart

[Run]
Filename: "{app}\fastrans.exe"; Description: "现在启动 fastrans"; Flags: nowait postinstall skipifsilent

[Code]
// Stop a running fastrans before installing over it or uninstalling.
procedure KillApp();
var
  R: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM fastrans.exe',
       '', SW_HIDE, ewWaitUntilTerminated, R);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  KillApp();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  KillApp();
  Result := True;
end;
