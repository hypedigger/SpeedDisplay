@echo off
REM Launch Lumen in development mode (hot reload)
cd /d "%~dp0"
call npm run tauri dev
pause
