# Lumen - one-time setup for Windows 11.
# Run from an elevated PowerShell:  powershell -ExecutionPolicy Bypass -File .\setup-windows.ps1
# Installs: Rust (MSVC), VS Build Tools (C++ linker), Node.js LTS, FFmpeg.

$ErrorActionPreference = "Continue"

function Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }

Step "Visual Studio Build Tools (C++ toolchain, required by Rust)"
winget install --id Microsoft.VisualStudio.2022.BuildTools --silent --accept-package-agreements --accept-source-agreements --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

Step "Rust (rustup, MSVC toolchain)"
winget install --id Rustlang.Rustup --silent --accept-package-agreements --accept-source-agreements

Step "Node.js LTS"
winget install --id OpenJS.NodeJS.LTS --silent --accept-package-agreements --accept-source-agreements

Step "FFmpeg (thumbnails, previews, remux)"
winget install --id Gyan.FFmpeg --silent --accept-package-agreements --accept-source-agreements

Step "Done"
Write-Host ""
Write-Host "Close this window, open a NEW terminal (so PATH is refreshed), then:" -ForegroundColor Yellow
Write-Host "    cd lumen"
Write-Host "    npm install"
Write-Host "    run-dev.cmd          (development, hot reload)"
Write-Host "    build-release.cmd    (optimised installer .exe)"
