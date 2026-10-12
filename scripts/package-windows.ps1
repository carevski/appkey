# Build appkey.exe, the Inno Setup installer and a portable .zip.
# Usage: scripts\package-windows.ps1 -Version 1.2.3   → dist\
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

cargo build --release
if ($LASTEXITCODE) { throw 'cargo build failed' }
New-Item -ItemType Directory -Force dist | Out-Null

$iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
if (-not (Test-Path $iscc)) { throw "Inno Setup 6 not found at $iscc (choco install innosetup)" }
& $iscc /Qp "/DAppVersion=$Version" packaging\windows\appkey.iss
if ($LASTEXITCODE) { throw 'ISCC failed' }

$stage = "target\package-windows\appkey-$Version-windows-x64"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\appkey.exe, README.md $stage
Copy-Item LICENSE "$stage\LICENSE.txt"
Compress-Archive -Force -Path $stage -DestinationPath "dist\appkey-$Version-windows-x64.zip"
Get-ChildItem dist
