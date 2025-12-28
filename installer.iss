[Setup]
AppName=capscr
AppVersion=0.1.0
AppPublisher=capscr
AppPublisherURL=https://github.com/lintowe/capscr
SetupIconFile=icon.ico
DefaultDirName={autopf}\capscr
DefaultGroupName=capscr
OutputBaseFilename=capscr-setup
Compression=lzma2/max
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64
WizardStyle=modern
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=Output

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "startupicon"; Description: "Start with Windows"; GroupDescription: "Startup:"

[Files]
Source: "target\release\capscr.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\capscr"; Filename: "{app}\capscr.exe"; IconFilename: "{app}\icon.ico"
Name: "{group}\Uninstall capscr"; Filename: "{uninstallexe}"
Name: "{autodesktop}\capscr"; Filename: "{app}\capscr.exe"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon
Name: "{userstartup}\capscr"; Filename: "{app}\capscr.exe"; Tasks: startupicon

[Run]
Filename: "{app}\capscr.exe"; Description: "Launch capscr"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
Type: filesandordirs; Name: "{userappdata}\capscr"
