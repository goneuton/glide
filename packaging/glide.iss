; Glide installer (Inno Setup 6). Build with: ISCC packaging\glide.iss
;
; Deliberately minimal: no pages to click through. It installs for the current
; user (no administrator rights), starts Glide, and Glide itself says Namaste and
; walks the user through setup (look, city, notifications, startup).
#define AppVersion "0.3.1"

[Setup]
AppId={{3C7B4B1E-8E2A-4F0B-9B55-6D1A2E7C4F21}
AppName=Glide
AppVersion={#AppVersion}
AppPublisher=Glide
AppVerName=Glide
DefaultDirName={autopf}\Glide
DefaultGroupName=Glide
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
OutputDir=..\dist
OutputBaseFilename=Glide-Setup-{#AppVersion}-x64
SetupIconFile=..\assets\glide.ico
UninstallDisplayIcon={app}\glide.exe
UninstallDisplayName=Glide
WizardStyle=modern
WizardSmallImageFile=..\assets\setup-small.bmp
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
ShowLanguageDialog=no
Compression=lzma2/max
SolidCompression=yes
CloseApplications=force
RestartApplications=no

[Messages]
SetupWindowTitle=Glide
WizardInstalling=Getting Glide ready
InstallingLabel=One moment. Glide will say hello at the top of your screen.

[Files]
Source: "..\target\release\glide.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\fonts\GeistMono-OFL.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
; The identity package (instant notifications), its public certificate, and the
; script Glide runs when the user turns instant notifications on during setup.
Source: "identity\Glide.msix"; DestDir: "{app}\identity"; Flags: ignoreversion
Source: "identity\Glide.cer"; DestDir: "{app}\identity"; Flags: ignoreversion
Source: "register-identity.ps1"; DestDir: "{app}\identity"; Flags: ignoreversion

[Icons]
Name: "{group}\Glide"; Filename: "{app}\glide.exe"

[Run]
; Start Glide straight away; it greets the user and runs its own setup.
Filename: "{app}\glide.exe"; Flags: nowait

[UninstallRun]
Filename: "{cmd}"; Parameters: "/C taskkill /IM glide.exe /F"; Flags: runhidden; RunOnceId: "StopGlide"
Filename: "powershell.exe"; Parameters: "-NoProfile -WindowStyle Hidden -Command ""Get-AppxPackage Glide.Island | Remove-AppxPackage"""; Flags: runhidden; RunOnceId: "RemoveIdentity"

[UninstallDelete]
Type: filesandordirs; Name: "{app}\identity"

[Registry]
; Start with Windows is chosen in Glide's setup; uninstalling removes it.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Glide"; Flags: dontcreatekey uninsdeletevalue