; Classic Windows setup file. Build with Inno Setup 6 (free): https://jrsoftware.org/isinfo.php
;   cargo build --release
;   iscc packaging\windows\ferrous-commander.iss
; The result is packaging\windows\Output\FerrousCommander-Setup.exe

#define AppName "Ferrous Commander"
; build-windows.ps1 passes the version from Cargo.toml.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppExe "ferrous-commander.exe"
#ifndef ReleaseExecutable
  #define ReleaseExecutable "..\..\target\release\ferrous-commander.exe"
#endif

[Setup]
AppId={{6C1E7C52-7A5B-4C4E-9F3B-2B8C1F0E6A11}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Ferrous Commander contributors
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Installs for the current user without admin rights; offers all-users when elevated.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputBaseFilename=FerrousCommander-Setup
SetupIconFile=ferrous-commander.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#ReleaseExecutable}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
