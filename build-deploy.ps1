# Build the release exe in place.
#
# A running SpeedDisplay is never closed and nothing is launched afterwards:
# the app belongs to whoever is using it. While an instance runs, its exe is
# locked, so the build only compiles (frontend + Rust) to check that everything
# builds, and says so; close the app and run the script again to replace it.
$ErrorActionPreference = "Continue"
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
Set-Location $PSScriptRoot
$exe = Join-Path $PSScriptRoot "src-tauri\target\release\SpeedDisplay.exe"

$running = @(Get-Process -Name SpeedDisplay, lumen -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    Write-Output ("SpeedDisplay is running (PID " + (($running | ForEach-Object Id) -join ", ") + "): left untouched.")
    npm run build 2>&1 | Select-String -Pattern "error|Error" | ForEach-Object { $_.Line }
    $front = $LASTEXITCODE
    Push-Location src-tauri
    cargo build --release 2>&1 | Select-String -Pattern "^error|Finished" | ForEach-Object { $_.Line }
    $back = $LASTEXITCODE
    Pop-Location
    if ($front -eq 0 -and $back -eq 0) {
        Write-Output "BUILD CHECKED - not deployed: close SpeedDisplay and run again to replace the exe"
        exit 0
    }
    Write-Output "BUILD FAILED"
    exit 1
}

npm run tauri build 2>&1 | Select-String -Pattern "Built application|error|Error" | ForEach-Object { $_.Line }
if ($LASTEXITCODE -eq 0 -and (Test-Path $exe)) {
    ie4uinit.exe -show
    Write-Output "DEPLOY OK - not launched"
} else {
    Write-Output "DEPLOY FAILED"
    exit 1
}
