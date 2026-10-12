; Inno Setup script for the Windows installer. Built by scripts/package-windows.ps1:
;   ISCC.exe /DAppVersion=1.2.3 packaging\windows\appkey.iss
; Installs per-user (no admin prompt) into %LOCALAPPDATA%\Programs\appkey.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{D16C446A-1663-422F-A5E8-0271B0720D6C}
AppName=appkey
AppVersion={#AppVersion}
AppVerName=appkey {#AppVersion}
AppPublisher=Vladimir Carevski
AppPublisherURL=https://github.com/carevski/appkey
AppSupportURL=https://github.com/carevski/appkey/issues
DefaultDirName={autopf}\appkey
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
OutputDir=..\..\dist
OutputBaseFilename=appkey-{#AppVersion}-windows-x64-setup
UninstallDisplayIcon={app}\appkey.exe
UninstallDisplayName=appkey
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
CloseApplications=force

[Tasks]
Name: "autostart"; Description: "Start appkey when I sign in"; GroupDescription: "Options:"
Name: "addtopath"; Description: "Add appkey to PATH (to use ""appkey list"" etc. in a terminal)"; GroupDescription: "Options:"; Flags: unchecked

[Files]
Source: "..\..\target\release\appkey.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\appkey"; Filename: "{app}\appkey.exe"
Name: "{userstartup}\appkey"; Filename: "{app}\appkey.exe"; Tasks: autostart

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\appkey.exe"; Description: "Start appkey now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM appkey.exe /F"; Flags: runhidden; RunOnceId: "StopAppkey"

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Dir: string;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then exit;
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then exit;
  Dir := ExpandConstant('{app}');
  P := Pos(';' + Uppercase(Dir), Uppercase(Path));
  if P > 0 then
  begin
    Delete(Path, P, Length(Dir) + 1);
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path);
  end;
end;
