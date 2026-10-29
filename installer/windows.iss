; Windows installer for LscSync (Inno Setup 6).
; Build: ISCC /DVersion=X.Y.Z installer\windows.iss (after the release build).
; Running a newer installer replaces the installed version (same AppId) and
; keeps the user's profile, which lives in %APPDATA%\LscSync.

#ifndef Version
  #error "Pass the version: ISCC /DVersion=X.Y.Z"
#endif

#define Name "LscSync"
#define DisplayName "LscSync"
#define Exe "lscsync.exe"

[Setup]
; Never change the AppId: it is what lets an installer replace an older version.
AppId={{8C44332A-12AA-4BF2-840D-E5728FE2DD87}
AppName={#DisplayName}
AppVersion={#Version}
AppVerName={#DisplayName} {#Version}
AppPublisher=Jonathan Gromat
AppPublisherURL=https://github.com/jonag972/LscSync
VersionInfoVersion={#Version}
; Per-user install: no administrator rights needed.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\{#Name}
DefaultGroupName={#DisplayName}
DisableProgramGroupPage=yes
DisableDirPage=auto
UninstallDisplayIcon={app}\{#Exe}
UninstallDisplayName={#DisplayName}
SetupIconFile=..\assets\icon\lscsync.ico
OutputDir=..\target\release\bundle
OutputBaseFilename={#Name}-{#Version}-windows-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Side panel and header art (tools/generate-icons.sh): Setup picks the size
; closest to the display scale.
WizardImageFile=..\assets\installer\wizard-image-202x386.png,..\assets\installer\wizard-image-336x643.png,..\assets\installer\wizard-image-534x1022.png
WizardSmallImageFile=..\assets\installer\wizard-small-58.png,..\assets\installer\wizard-small-97.png,..\assets\installer\wizard-small-159.png
; Windows 10 or later.
MinVersion=10.0
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Closes the running app before replacing its files.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
fr.DesktopIcon=Créer une icône sur le Bureau
en.DesktopIcon=Create a desktop icon
fr.StartAtLogin=Lancer {#DisplayName} au démarrage de Windows
en.StartAtLogin=Start {#DisplayName} when Windows starts
fr.LaunchNow=Lancer {#DisplayName}
en.LaunchNow=Launch {#DisplayName}

[Tasks]
Name: "desktop"; Description: "{cm:DesktopIcon}"; Flags: unchecked
Name: "startup"; Description: "{cm:StartAtLogin}"

[Files]
Source: "..\target\release\{#Exe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#DisplayName}"; Filename: "{app}\{#Exe}"
Name: "{autodesktop}\{#DisplayName}"; Filename: "{app}\{#Exe}"; Tasks: desktop

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
    ValueName: "{#Name}"; ValueData: """{app}\{#Exe}"""; Tasks: startup; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#Exe}"; Description: "{cm:LaunchNow}"; Flags: nowait postinstall skipifsilent
; Silent install: an update started from the app, which restarts it.
Filename: "{app}\{#Exe}"; Flags: nowait; Check: WizardSilent

[UninstallRun]
; Stops the app before removing its files.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#Exe}"; Flags: runhidden; RunOnceId: "StopApp"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, RunKey, '{#Name}');
end;
