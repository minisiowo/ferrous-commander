# Builds the Windows release and installs it for the current user (no admin rights).
# Works from any folder:  powershell -ExecutionPolicy Bypass -File app\packaging\build-and-install-windows.ps1
$ErrorActionPreference = "Stop"

& (Join-Path $PSScriptRoot "build-windows.ps1")

$executable = Join-Path $PSScriptRoot "..\dist\FerrousCommander-windows-x64\ferrous-commander.exe"

# Piped so PowerShell waits for this GUI program and collects what it prints.
# Windows PowerShell turns redirected stderr into errors, which "Stop" would make fatal.
$ErrorActionPreference = "Continue"
$output = & $executable --install 2>&1 | Out-String
$exitCode = $LASTEXITCODE
$ErrorActionPreference = "Stop"

Write-Host $output.Trim()
if ($exitCode -ne 0) {
    throw "Install failed. If Ferrous Commander is running, close it and try again."
}
