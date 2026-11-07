# Builds the Windows release, a portable zip, and (when Inno Setup is installed) a setup .exe.
# Run from the app folder:  powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$metadataJson = cargo metadata --format-version 1 --no-deps --locked
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
$metadata = $metadataJson | ConvertFrom-Json
$executable = Join-Path $metadata.target_directory "release\ferrous-commander.exe"
if (-not (Test-Path -LiteralPath $executable)) { throw "Release executable not found: $executable" }

# Reject console builds before they can reach a portable archive or installer.
$peBytes = [System.IO.File]::ReadAllBytes($executable)
$peOffset = [BitConverter]::ToInt32($peBytes, 0x3c)
if ([BitConverter]::ToUInt32($peBytes, $peOffset) -ne 0x00004550) {
    throw "Not a Windows PE executable: $executable"
}
$subsystem = [BitConverter]::ToUInt16($peBytes, $peOffset + 24 + 68)
if ($subsystem -ne 2) {
    throw "Refusing to package a console application (PE subsystem $subsystem); expected Windows GUI (2)."
}

$dist = "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$zip = Join-Path $dist "FerrousCommander-windows-x64.zip"
Compress-Archive -Force -Path $executable -DestinationPath $zip
Write-Host "Portable zip: $zip  (unzip, run ferrous-commander.exe, or run it with --install)"

# Unpack next to the zip so the build can be started right away.
# Overwrite in place: the folder may be held open, and a locked exe must survive a failed build.
$unpacked = Join-Path $dist "FerrousCommander-windows-x64"
Expand-Archive -Force -Path $zip -DestinationPath $unpacked
Write-Host "Unpacked build: $(Join-Path $unpacked 'ferrous-commander.exe')"

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($iscc) {
    & $iscc "/DReleaseExecutable=$executable" "/DAppVersion=$($metadata.packages[0].version)" "packaging\windows\ferrous-commander.iss"
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup build failed" }
    Copy-Item "packaging\windows\Output\FerrousCommander-Setup.exe" $dist -Force
    Write-Host "Installer: $dist\FerrousCommander-Setup.exe"
} else {
    Write-Host "Inno Setup not found: skipped the setup .exe (winget install JRSoftware.InnoSetup)"
}
