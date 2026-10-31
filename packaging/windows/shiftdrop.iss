; ShiftDrop for Windows: installer (Inno Setup 6). Built by build-installer.ps1:
;   ISCC /DAppVersion=0.1.0 /DSourceDir=<staged files> packaging\windows\shiftdrop.iss
;
; Installs shiftdrop.exe (the app) and shiftdrop-cli.exe, Ghostscript for PDF
; compression, a Send To entry and start-at-login, and offers to fetch the
; other converters with winget.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6F1C8E2B-5B0E-4C55-9C51-2D7A0E3F8A41}
AppName=ShiftDrop
AppVersion={#AppVersion}
AppPublisher=Jay Hemnani
AppPublisherURL=https://github.com/jayhemnani9910/shiftdrop-rust
DefaultDirName={autopf}\ShiftDrop
DefaultGroupName=ShiftDrop
DisableProgramGroupPage=yes
; admin: winget installs the converters for every user, and needs to
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputBaseFilename=ShiftDrop-Setup-x64
OutputDir={#SourceDir}\..
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile={#SourceDir}\LICENSE
UninstallDisplayIcon={app}\shiftdrop.exe
; the converters land on PATH; tell running programs
ChangesEnvironment=yes
CloseApplications=force

[Tasks]
Name: startup; Description: "Start ShiftDrop when I sign in (needed for Shift-drag)"
Name: sendto; Description: "Add ShiftDrop to Explorer's Send To menu"
Name: ffmpeg; Description: "FFmpeg (video and audio)"; GroupDescription: "Install converters with winget:"
Name: imagemagick; Description: "ImageMagick (images)"; GroupDescription: "Install converters with winget:"
Name: poppler; Description: "Poppler (PDF pages and text)"; GroupDescription: "Install converters with winget:"
Name: libreoffice; Description: "LibreOffice (PDF and text to Word; large)"; GroupDescription: "Install converters with winget:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\shiftdrop.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\shiftdrop-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"
Source: "{#SourceDir}\README.md"; DestDir: "{app}"
; Ghostscript (AGPL; source: https://github.com/ArtifexSoftware/ghostpdl), found by the app here
Source: "{#SourceDir}\tools\ghostscript\*"; DestDir: "{app}\tools\ghostscript"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{autostartup}\ShiftDrop"; Filename: "{app}\shiftdrop.exe"; Tasks: startup
Name: "{usersendto}\ShiftDrop"; Filename: "{app}\shiftdrop.exe"; Tasks: sendto

[Run]
Filename: "winget"; Parameters: "install --id Gyan.FFmpeg {code:WingetFlags}"; StatusMsg: "Installing FFmpeg with winget..."; Flags: runhidden waituntilterminated; Tasks: ffmpeg; Check: HaveWinget
Filename: "winget"; Parameters: "install --id ImageMagick.ImageMagick {code:WingetFlags}"; StatusMsg: "Installing ImageMagick with winget..."; Flags: runhidden waituntilterminated; Tasks: imagemagick; Check: HaveWinget
Filename: "winget"; Parameters: "install --id oschwartz10612.Poppler {code:WingetFlags}"; StatusMsg: "Installing Poppler with winget..."; Flags: runhidden waituntilterminated; Tasks: poppler; Check: HaveWinget
Filename: "winget"; Parameters: "install --id TheDocumentFoundation.LibreOffice {code:WingetFlags}"; StatusMsg: "Installing LibreOffice with winget (this takes a while)..."; Flags: runhidden waituntilterminated; Tasks: libreoffice; Check: HaveWinget
; the app must run as the signed-in user, not as admin: Windows keeps drags
; from normal apps away from an elevated window
Filename: "{app}\shiftdrop.exe"; Description: "Start ShiftDrop now"; Flags: nowait postinstall runasoriginaluser

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM shiftdrop.exe"; Flags: runhidden; RunOnceId: "StopShiftDrop"

[Code]
var
  Winget: Integer; { 0 not checked yet, 1 there, 2 missing }

function HaveWinget: Boolean;
var
  Code: Integer;
begin
  if Winget = 0 then
  begin
    if Exec('winget', '--version', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0) then
      Winget := 1
    else
      Winget := 2;
  end;
  Result := Winget = 1;
end;

function WingetFlags(Param: String): String;
begin
  Result := '-e --source winget --scope machine --silent --disable-interactivity ' +
            '--accept-package-agreements --accept-source-agreements';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and (WizardIsTaskSelected('ffmpeg') or WizardIsTaskSelected('imagemagick') or
     WizardIsTaskSelected('poppler') or WizardIsTaskSelected('libreoffice')) and not HaveWinget then
    MsgBox('winget isn''t available on this PC, so the converters weren''t installed.' + #13#10 + #13#10 +
           'Get them by hand: FFmpeg (gyan.dev/ffmpeg), ImageMagick (imagemagick.org), Poppler ' +
           '(github.com/oschwartz10612/poppler-windows), LibreOffice (libreoffice.org). ShiftDrop offers ' +
           'each format once its converter is found.', mbInformation, MB_OK);
end;
