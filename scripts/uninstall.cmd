@echo off
rem Removes win-app-switcher (see README "Uninstall"): exits the switcher,
rem deletes the logon task, the install folder, and this account's
rem Startup-folder shortcut. Run from an elevated prompt.

net session >nul 2>&1 || (
    echo Run this from an elevated prompt.
    pause & exit /b 1
)

taskkill /IM win-app-switcher.exe /F >nul 2>&1
schtasks /Delete /TN win-app-switcher /F >nul 2>&1
rmdir /S /Q "C:\Program Files\win-app-switcher" 2>nul
del "%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\win-app-switcher.lnk" 2>nul

if exist "C:\Program Files\win-app-switcher" (
    echo Could not delete "C:\Program Files\win-app-switcher" - remove it manually.
    pause & exit /b 1
)
echo win-app-switcher removed.
pause
