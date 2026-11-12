; Inno Setup script for the Lumen Windows installer. Built by build-setup.sh:
;   ISCC /DStageDir=<staged dir> /DAppVersion=<CalVer> /DIconFile=<lumen-text.ico> /DOutDir=<dir> /DOutName=<base name> lumen-text.iss
; Optional: /DVersionNumeric=a.b.c.d (file version), /DOutDir, /DOutName. StageDir is the directory produced by stage.sh (the same file list as the portable zip).

#ifndef StageDir
  #error StageDir is not defined (the staged lumen-text.exe + assets directory)
#endif
#ifndef AppVersion
  #error AppVersion is not defined
#endif
#ifndef IconFile
  #error IconFile is not defined
#endif
#ifndef VersionNumeric
  #define VersionNumeric AppVersion
#endif
#ifndef OutDir
  #define OutDir "."
#endif
#ifndef OutName
  #define OutName "lumen-text-" + AppVersion + "-windows-x86_64-setup"
#endif

#define AppName "Lumen"
#define AppExe "lumen-text.exe"
#define AppURL "https://github.com/castrogusttavo/lumen-text"

[Setup]
; Stable: never change it, so a new version installs over the old one and keeps the uninstall entry.
AppId={{B7C1E0D4-5A3F-4C8E-9E21-6F0A7D3C41B2}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Lumen
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
VersionInfoVersion={#VersionNumeric}
VersionInfoDescription=Lumen setup
; Per user by default (%LOCALAPPDATA%\Programs\Lumen, no admin); the dialog offers "all users".
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline dialog
DefaultDirName={autopf}\Lumen
UsePreviousAppDir=yes
DisableProgramGroupPage=yes
DisableDirPage=auto
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\lumen-text.ico
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
OutputDir={#OutDir}
OutputBaseFilename={#OutName}
; Upgrades: ask Windows (Restart Manager) to close a running Lumen, and never restart the PC.
CloseApplications=yes
CloseApplicationsFilter=lumen-text.exe,ghp-lsp.exe
RestartApplications=no
; The PATH and file-association tasks tell Explorer and other programs to re-read their settings.
ChangesEnvironment=yes
ChangesAssociations=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "addtopath"; Description: "Add Lumen to the user &PATH (run ""lumen-text"" from a terminal)"; GroupDescription: "Integration:"
Name: "contextmenu"; Description: "Add ""Open with Lumen"" to the file menu and ""Open folder in Lumen"" to the folder menus"; GroupDescription: "Integration:"
Name: "fileassoc"; Description: "Offer Lumen in ""Open with"" for common source and text files"; GroupDescription: "Integration:"; Flags: unchecked

[InstallDelete]
; A new version replaces assets/ wholesale (files removed upstream must not linger).
Type: filesandordirs; Name: "{app}\assets"

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion
Source: "{#IconFile}"; DestDir: "{app}"; DestName: "lumen-text.ico"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\lumen-text.ico"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\lumen-text.ico"; Tasks: desktopicon

[Registry]
; User PATH (HKCU\Environment); removed again by the uninstaller (see CurUninstallStepChanged).
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath

; Context menus. HKA is HKCU for a per-user install and HKLM for an all-users one.
Root: HKA; Subkey: "Software\Classes\*\shell\Lumen"; ValueType: string; ValueName: ""; ValueData: "Open with Lumen"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\*\shell\Lumen"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\lumen-text.ico"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\*\shell\Lumen\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Lumen"; ValueType: string; ValueName: ""; ValueData: "Open folder in Lumen"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Lumen"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\lumen-text.ico"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Lumen\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Lumen"; ValueType: string; ValueName: ""; ValueData: "Open folder in Lumen"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Lumen"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\lumen-text.ico"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Lumen\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%V"""; Tasks: contextmenu

; "Open with" entries (the default program of a file type is never taken over).
Root: HKA; Subkey: "Software\Classes\Lumen.File"; ValueType: string; ValueName: ""; ValueData: "Lumen document"; Flags: uninsdeletekey; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\Lumen.File\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\lumen-text.ico"; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\Lumen.File\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.txt\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.md\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.json\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.toml\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.yaml\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.yml\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.rs\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.ts\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.tsx\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.js\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.jsx\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.go\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.py\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.html\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.css\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.c\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.h\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.cpp\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.sh\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc
Root: HKA; Subkey: "Software\Classes\.ghp\OpenWithProgids"; ValueType: string; ValueName: "Lumen.File"; ValueData: ""; Flags: uninsdeletevalue; Tasks: fileassoc

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch Lumen"; Flags: nowait postinstall skipifsilent
; The in-app updater runs this setup silently with /RELAUNCH=1 and expects Lumen to come back.
Filename: "{app}\{#AppExe}"; Flags: nowait runasoriginaluser; Check: RelaunchRequested

[Code]
const
  EnvKey = 'Environment';

function RelaunchRequested: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;

function NeedsAddPath(Dir: string): Boolean;
var
  Cur: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Cur) then
  begin
    Result := True;
    Exit;
  end;
  Result := Pos(';' + Lowercase(Dir) + ';', ';' + Lowercase(Cur) + ';') = 0;
end;

procedure RemoveFromPath(Dir: string);
var
  Cur, Wrapped, Needle: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Cur) then Exit;
  Wrapped := ';' + Cur + ';';
  Needle := ';' + Lowercase(Dir) + ';';
  P := Pos(Needle, Lowercase(Wrapped));
  if P = 0 then Exit;
  Delete(Wrapped, P, Length(Needle) - 1);   { keep one ';' between the neighbours }
  Cur := Copy(Wrapped, 2, Length(Wrapped) - 2);
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Cur);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
  if (CurUninstallStep = usPostUninstall) and (not UninstallSilent) then
  begin
    { Settings, session and caches live outside the install directory; keep them unless asked. }
    if MsgBox('Also delete your Lumen settings, sessions and caches?' + #13#10#13#10 +
              ExpandConstant('{userappdata}\lumen-text') + #13#10 +
              ExpandConstant('{localappdata}\lumen-text') + #13#10#13#10 +
              'Choose No to keep them for a later reinstall.',
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\lumen-text'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\lumen-text'), True, True, True);
    end;
  end;
end;
