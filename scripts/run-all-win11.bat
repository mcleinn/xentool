@echo off
REM Open Windows Terminal with three tabs running xentool, xenharm and
REM SuperCollider in one window. Falls back to three separate cmd
REM windows if Windows Terminal can't be located.
REM
REM Tab order — leftmost first:
REM   1. xentool        (focused; waits 2 s so xenharm has bound)
REM   2. xenharm        (starts immediately)
REM   3. supercollider  (waits 5 s so xentool MIDI / loopMIDI are up)
REM
REM xentool resumes the last-used layout. To pin a specific layout,
REM edit the `xentool serve` line below.

setlocal EnableDelayedExpansion
set "REPO=%~dp0.."

REM --- locate wt.exe ---
REM 1) PATH lookup (covers most installs)
REM 2) the standard App Execution Alias shim in %LocalAppData%
REM 3) the actual UWP install under %ProgramFiles%\WindowsApps\Microsoft.WindowsTerminal_*
REM 4) fall back to PowerShell (Get-Command), which sees aliases cmd often misses
set "WT="
where wt >nul 2>&1 && set "WT=wt"

if not defined WT (
    if exist "%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe" (
        set "WT=%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe"
    )
)

if not defined WT (
    REM Find the highest-versioned UWP install. Skip silently if the dir
    REM isn't readable (default ACLs hide WindowsApps for non-admins, but
    REM individual package dirs are usually still openable).
    for /f "delims=" %%P in ('dir /b /a:d "%ProgramFiles%\WindowsApps\Microsoft.WindowsTerminal_*" 2^>nul') do (
        if exist "%ProgramFiles%\WindowsApps\%%P\wt.exe" (
            set "WT=%ProgramFiles%\WindowsApps\%%P\wt.exe"
        )
    )
)

if not defined WT (
    REM PowerShell sees App Execution Aliases that cmd's `where` doesn't.
    for /f "usebackq delims=" %%P in (`powershell -NoProfile -Command "(Get-Command wt -ErrorAction SilentlyContinue).Source"`) do (
        if exist "%%P" set "WT=%%P"
    )
)

if defined WT (
    echo Using Windows Terminal: !WT!
    REM `;` separates tabs in the wt command — escape as `^;` for cmd.exe.
    REM `focus-tab -t 0` brings the leftmost tab (xentool) into focus.
    "!WT!" -w 0 ^
        new-tab --title "xentool"        -d "%REPO%"                 cmd /k "timeout /t 2 /nobreak >nul && xentool serve --hud" ^
^;      new-tab --title "xenharm"        -d "%REPO%\xenharm_service" cmd /k "python server.py" ^
^;      new-tab --title "supercollider"  -d "%REPO%\scripts"         cmd /k "timeout /t 5 /nobreak >nul && start-supercollider.bat" ^
^;      focus-tab -t 0
    exit /b 0
)

REM --- fallback: three separate cmd windows ---
echo Windows Terminal (wt.exe) not found via PATH, %%LOCALAPPDATA%%\Microsoft\WindowsApps,
echo %%ProgramFiles%%\WindowsApps, or PowerShell Get-Command. Falling back to
echo three separate cmd windows. Install Windows Terminal from the Microsoft
echo Store for the nicer single-window-three-tabs layout.
echo.
start "xenharm"       cmd /k "cd /d ""%REPO%\xenharm_service"" && python server.py"
start "xentool"       cmd /k "cd /d ""%REPO%"" && timeout /t 2 /nobreak >nul && xentool serve --hud"
start "supercollider" cmd /k "cd /d ""%REPO%\scripts"" && timeout /t 5 /nobreak >nul && start-supercollider.bat"
exit /b 0
