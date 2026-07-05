@echo off
rem Dev cycle: kill the running switcher, copy the fresh debug build from
rem this repo (usually on a network share) to a local folder and start it
rem elevated. The local copy matters: an elevated process cannot use the
rem user's share mappings, and Windows can serve a stale exe image from a
rem share while an old instance still runs.

rem Killing an elevated process needs an elevated taskkill, so the script
rem self-elevates once - one UAC prompt covers both kill and start.
rem (fltmc is the admin probe: it fails without elevation, needs no service.)
fltmc >nul 2>&1 || (
    powershell -NoProfile Start-Process -Verb RunAs -FilePath '%~f0'
    exit /b
)

rem The elevated session lacks the share's drive letter; pushd maps %~dp0
rem to a temporary one (works for UNC paths too), popd cleans it up.
pushd "%~dp0" || (echo Cannot reach %~dp0 & pause & exit /b 1)

rem After a forced kill, give the image lock a moment to release.
taskkill /IM win-app-switcher.exe /F >nul 2>&1 && timeout /t 1 /nobreak >nul

set "DEST=%LOCALAPPDATA%\win-app-switcher"
if not exist "%DEST%" mkdir "%DEST%"
copy /Y "..\target\x86_64-pc-windows-msvc\debug\win-app-switcher.exe" "%DEST%" >nul || (
    echo Copy failed - has 'make debug' been run?
    popd & pause & exit /b 1
)
rem config.toml is read from the exe's folder; keep a repo-root one in sync.
if exist "config.toml" copy /Y "config.toml" "%DEST%" >nul
popd

start "" "%DEST%\win-app-switcher.exe"
