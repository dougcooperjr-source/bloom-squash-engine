@echo off
cd /d "%~dp0"
echo Bloom Squash Engine - Windows builder
echo.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0BUILD_WINDOWS_v2.ps1"
echo.
pause
