# Build the release exe and relaunch it. Retries once if the binary was
# locked by a running instance during the final rename step.
$ErrorActionPreference = "Continue"
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
Set-Location $PSScriptRoot
$exe = Join-Path $PSScriptRoot "src-tauri\target\release\SpeedDisplay.exe"

function Kill-App {
    taskkill /IM SpeedDisplay.exe /F 2>$null | Out-Null
    taskkill /IM lumen.exe /F 2>$null | Out-Null
    Start-Sleep -Seconds 1
}

$success = $false
foreach ($attempt in 1, 2) {
    Kill-App
    npm run tauri build 2>&1 | Select-String -Pattern "Built application|error|Error" | ForEach-Object { $_.Line }
    if ($LASTEXITCODE -eq 0 -and (Test-Path $exe)) {
        $success = $true
        break
    }
    Write-Output "BUILD ATTEMPT $attempt FAILED (exit $LASTEXITCODE), retrying..."
}

if ($success) {
    ie4uinit.exe -show
    Start-Process -FilePath $exe
    Write-Output "DEPLOY OK"
} else {
    Write-Output "DEPLOY FAILED"
    exit 1
}
