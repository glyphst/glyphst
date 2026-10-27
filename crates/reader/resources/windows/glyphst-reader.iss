; The Glyphst Reader installer for Windows (Inno Setup 6.3 or newer), built
; by script/bundle:
;
;   ISCC /DVersion=26.10.25.1 /DFileVersion=26.10.25.1 /DArch=x86_64|aarch64
;        /DSourceDir=<staged files> /DOutputDir=<dir> /DOutputBaseFilename=<name>
;        glyphst-reader.iss
;
; It installs for the current user, with no administrator prompt, into
; %LOCALAPPDATA%\Programs\Glyphst Reader: a Start menu entry, glyphst://
; links, and as tasks a desktop shortcut and Open With for PDF and EPUB
; (never made the default; Windows asks).

#ifndef Version
  #error Pass /DVersion
#endif
#ifndef FileVersion
  #error Pass /DFileVersion
#endif
#ifndef Arch
  #error Pass /DArch
#endif
#ifndef SourceDir
  #error Pass /DSourceDir
#endif
#ifndef OutputDir
  #error Pass /DOutputDir
#endif
#ifndef OutputBaseFilename
  #error Pass /DOutputBaseFilename
#endif

#define AppName "Glyphst Reader"
#define AppExe "glyphst-reader.exe"
#define Capabilities "Software\Glyphst\Glyphst Reader\Capabilities"

[Setup]
; The same for every release, so a new one replaces the installed one.
AppId={{66AD7189-4588-4365-8B11-97106F7F7C4D}
AppName={#AppName}
AppVersion={#Version}
AppVerName={#AppName} {#Version}
AppPublisher=Glyphst
AppPublisherURL=https://glyphst.com
AppSupportURL=https://github.com/glyphst/glyphst/issues
AppUpdatesURL=https://github.com/glyphst/glyphst/releases
VersionInfoVersion={#FileVersion}
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
#if Arch == "aarch64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif
MinVersion=10.0
WizardStyle=modern
SetupIconFile={#SourceDir}\glyphst-reader.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
Compression=lzma2/max
SolidCompression=yes
ChangesAssociations=yes
CloseApplications=yes
; Not signed yet; with a certificate, a sign tool named in ISCC's /S
; signs the installer and uninstaller:
; SignTool=signtool

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "associate"; Description: "Offer {#AppName} in Open with for PDF and EPUB documents"; GroupDescription: "Documents:"

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; `glyphst-reader` from Run (Win+R).
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\{#AppExe}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe}"; Flags: uninsdeletekey

; glyphst:// citations from other apps.
Root: HKA; Subkey: "Software\Classes\glyphst"; ValueType: string; ValueName: ""; ValueData: "URL:Glyphst link"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\glyphst"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\glyphst\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Classes\glyphst\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""

; Open With for PDF and EPUB: a ProgID for each, listed under the
; extension, and the Reader among the Default apps.
Root: HKA; Subkey: "Software\Classes\GlyphstReader.pdf"; ValueType: string; ValueName: ""; ValueData: "PDF document"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GlyphstReader.pdf\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GlyphstReader.pdf\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: string; ValueName: "GlyphstReader.pdf"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GlyphstReader.epub"; ValueType: string; ValueName: ""; ValueData: "EPUB document"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GlyphstReader.epub\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GlyphstReader.epub\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.epub\OpenWithProgids"; ValueType: string; ValueName: "GlyphstReader.epub"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#AppName}"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".pdf"; ValueData: ""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".epub"; ValueData: ""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: associate
Root: HKA; Subkey: "Software\Glyphst"; Flags: uninsdeletekeyifempty; Tasks: associate
Root: HKA; Subkey: "Software\Glyphst\Glyphst Reader"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "{#Capabilities}"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"; Tasks: associate
Root: HKA; Subkey: "{#Capabilities}"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Read PDF and EPUB documents"; Tasks: associate
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".pdf"; ValueData: "GlyphstReader.pdf"; Tasks: associate
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".epub"; ValueData: "GlyphstReader.epub"; Tasks: associate
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#AppName}"; ValueData: "{#Capabilities}"; Flags: uninsdeletevalue; Tasks: associate

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
