; MossRaven installer (Inno Setup 6 — https://jrsoftware.org/isdl.php)
; Build:  1) assemble dist\ (cargo build --release; dotnet publish; copy sidecars)
;         2) iscc installer\MossRaven.iss
; Output: installer\Output\MossRaven-Setup-<version>.exe
;
; API keys: NOT collected by the installer. First launch opens the app;
; the user adds free-tier keys (Cerebras / Groq / Gemini) or an Anthropic
; key via the Settings gear — stored per-user in settings.json, never
; machine-wide. The post-install page links the key signup URLs.

#define AppName "MossRaven"
#define AppVersion "0.2.0"
#define AppPublisher "Moss Softworks"
#define AppURL "https://github.com/MossSoftworks/MossRaven"

[Setup]
AppId={{7E1B5A2D-90C4-4C2B-A1F3-MOSSRAVEN02}}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir=Output
OutputBaseFilename=MossRaven-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
SetupIconFile=..\ui\MossRaven\Assets\mossraven.ico
UninstallDisplayIcon={app}\MossRaven.exe
; Code signing (when a cert exists): uncomment SignTool and configure in the
; Inno IDE: Tools -> Configure Sign Tools ->
;   signtool=$qC:\path\signtool.exe$q sign /fd SHA256 /a /t http://timestamp.digicert.com $f
;SignTool=signtool

[Files]
Source: "..\dist\MossRaven.exe";            DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\mossraven-service.exe";    DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\mossraven-node.exe";       DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\dist\seed.xml";                 DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
; PoB2 data: the engine needs a PathOfBuilding-PoE2 checkout. Per GGG
; fan-content policy we do NOT redistribute it; first launch without it
; runs in disconnected mode and the README explains the one-line clone.
Source: "..\scripts\corpus-churn.ps1";      DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "..\scripts\train-value-model.py";  DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "..\README.md";                     DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\MossRaven.exe"
Name: "{autodesktop}\{#AppName}";  Filename: "{app}\MossRaven.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Run]
Filename: "{app}\MossRaven.exe"; Description: "Launch {#AppName} (add your free API keys in Settings)"; Flags: nowait postinstall skipifsilent

[Messages]
FinishedLabel=Setup is complete.%n%nFirst steps:%n1. Launch MossRaven and open Settings (gear icon).%n2. Add at least one free API key:  Cerebras (cloud.cerebras.ai)  ·  Groq (console.groq.com/keys)  ·  Google AI Studio (aistudio.google.com/apikey).%n3. Optional: set your Path of Building 2 install path for the workspace's "Open in PoB2" button.
