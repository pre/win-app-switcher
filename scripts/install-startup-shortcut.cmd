@echo off
rem For standard user accounts: a Task Scheduler logon task with "Run with
rem highest privileges" only elevates members of the Administrators group
rem (see README "Start at login"). This creates a Startup-folder shortcut
rem instead: at every logon UAC asks for administrator credentials and
rem starts the switcher elevated - Cancel starts it unelevated.
rem Run this as the account that logs in; no elevation needed.

set "EXE=C:\Program Files\win-app-switcher\win-app-switcher.exe"
if not exist "%EXE%" (
    echo %EXE% not found - install win-app-switcher there first.
    pause & exit /b 1
)

rem The shortcut targets powershell, not the exe: a Startup item that itself
rem requires elevation can be silently blocked at logon, but an unelevated
rem wrapper that *requests* elevation (Start-Process -Verb RunAs) is not.
powershell -NoProfile -Command ^
    "$exe = $env:EXE;" ^
    "$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut([Environment]::GetFolderPath('Startup') + '\win-app-switcher.lnk');" ^
    "$lnk.TargetPath = 'powershell.exe';" ^
    "$lnk.Arguments = '-WindowStyle Hidden -Command try { Start-Process ''' + $exe + ''' -Verb RunAs -ErrorAction Stop } catch { Start-Process ''' + $exe + ''' }';" ^
    "$lnk.WindowStyle = 7;" ^
    "$lnk.IconLocation = $exe + ',0';" ^
    "$lnk.Save();" ^
    "Write-Host ('Created ' + $lnk.FullName)" || (pause & exit /b 1)

echo The switcher now starts at every logon after a UAC credentials prompt.
pause
