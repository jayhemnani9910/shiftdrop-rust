# Build ShiftDrop-Setup-x64.exe from a release build. Run on Windows from the
# repo root, after `cargo build --release`:
#   pwsh packaging/windows/build-installer.ps1 -Version 0.1.0
# Fetches Ghostscript (checked against a known hash), stages the files and runs
# Inno Setup. The installer lands in target/.
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'

$gsUrl = 'https://github.com/ArtifexSoftware/ghostpdl-downloads/releases/download/gs10080/gs10080w64.exe'
$gsSha256 = '52A91B8BF09298788D7A57B9206127026C23EACD75405F0A131E26DC381DCE50'

$stage = Join-Path $PWD 'target\installer'
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$stage\tools\ghostscript\bin" | Out-Null

Copy-Item target\release\shiftdrop.exe, target\release\shiftdrop-cli.exe, LICENSE, README.md $stage

# Ghostscript's own installer can't run silently any more; its DLL carries its
# resources, so the console exe and the DLL are all ShiftDrop needs
$temp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$gsSetup = Join-Path $temp 'gs-setup.exe'
Invoke-WebRequest $gsUrl -OutFile $gsSetup
$hash = (Get-FileHash $gsSetup -Algorithm SHA256).Hash
if ($hash -ne $gsSha256) { throw "Ghostscript download has hash $hash, expected $gsSha256" }
& 7z e -y "-o$stage\tools\ghostscript\bin" $gsSetup bin/gswin64c.exe bin/gsdll64.dll | Out-Null
if (-not (Test-Path "$stage\tools\ghostscript\bin\gsdll64.dll")) { throw 'Ghostscript files missing after extraction' }

$iscc = 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
& $iscc "/DAppVersion=$Version" "/DSourceDir=$stage" packaging\windows\shiftdrop.iss
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed ($LASTEXITCODE)" }
