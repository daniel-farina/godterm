; Inno Setup 6 script: GodTerm Windows installer (per user, no admin).
;   iscc /DMyAppVersion=0.2.0 /DPayload=<dir with godterm.exe> /DOutDir=<dist> packaging\windows\GodTerm.iss
; Built by scripts/release/windows.ps1.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef Payload
  #define Payload "payload"
#endif
#ifndef OutDir
  #define OutDir "..\..\dist"
#endif

#define MyAppName "GodTerm"
#define MyAppPublisher "Daniel Farina"
#define MyAppURL "https://github.com/daniel-farina/godterm"
#define MyAppExeName "godterm.exe"

[Setup]
AppId={{6E0B7C1A-3F52-4D8E-9A61-2C4B9D7E5F13}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
DefaultDirName={autopf}\GodTerm
DefaultGroupName=GodTerm
DisableProgramGroupPage=yes
LicenseFile={#Payload}\LICENSE.txt
OutputDir={#OutDir}
OutputBaseFilename=GodTerm-{#MyAppVersion}-windows-x64-setup
SetupIconFile=godterm.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ChangesEnvironment=yes
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "addtopath"; Description: "Add godterm to my PATH"; GroupDescription: "Command line:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Payload}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Opens in the default terminal (Windows Terminal on Windows 11).
Name: "{group}\GodTerm"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{userdocs}"
Name: "{group}\Uninstall GodTerm"; Filename: "{uninstallexe}"
Name: "{autodesktop}\GodTerm"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{userdocs}"; Tasks: desktopicon

[Registry]
; Per user PATH entry, removed again on uninstall.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Code]
function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, App: string;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then exit;
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then exit;
  App := ExpandConstant('{app}');
  P := Pos(';' + Uppercase(App), Uppercase(Path));
  if P > 0 then
  begin
    Delete(Path, P, Length(App) + 1);
    RegWriteStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path);
  end;
end;
