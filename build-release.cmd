@echo off
REM Build the Lumen release installer (NSIS)
REM Output: src-tauri\target\release\bundle\nsis\
cd /d "%~dp0"
call npm run tauri build
echo.
echo Installer available in src-tauri\target\release\bundle\nsis\
pause
