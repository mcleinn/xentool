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
REM Each tab calls one of the small `start-*.bat` / `_run-all-*.bat`
REM helpers next to this script — that keeps cmd /k arguments single
REM commands, no nested && / quoting.
REM
REM `-w new` always opens a NEW Windows Terminal window so any wt
REM session you already have running is not touched.

setlocal EnableDelayedExpansion
set "REPO=%~dp0.."

REM --- locate wt.exe ---
REM 1) PATH lookup (covers most installs).
REM 2) the standard App Execution Alias shim in %LocalAppData%.
REM 3) the actual UWP install under %ProgramFiles%\WindowsApps\Microsoft.WindowsTerminal_*.
REM 4) fall back to PowerShell (Get-Command), which sees aliases cmd often misses.
set "WT="
where wt >nul 2>&1 && set "WT=wt"

if not defined WT (
    if exist "%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe" (
        set "WT=%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe"
    )
)

if not defined WT (
    for /f "delims=" %%P in ('dir /b /a:d "%ProgramFiles%\WindowsApps\Microsoft.WindowsTerminal_*" 2^>nul') do (
        if exist "%ProgramFiles%\WindowsApps\%%P\wt.exe" (
            set "WT=%ProgramFiles%\WindowsApps\%%P\wt.exe"
        )
    )
)

if not defined WT (
    for /f "usebackq delims=" %%P in (`powershell -NoProfile -Command "(Get-Command wt -ErrorAction SilentlyContinue).Source"`) do (
        if exist "%%P" set "WT=%%P"
    )
)

if defined WT (
    echo Using Windows Terminal: !WT!
    REM `;` separates tabs in the wt command — escape as `^;` for cmd.exe.
    REM `-w new` opens a fresh wt window; `focus-tab -t 0` focuses xentool.
    "!WT!" -w new ^
        new-tab --title "xentool"        -d "%REPO%"                 cmd /k "%~dp0_run-all-xentool.bat" ^
^;      new-tab --title "xenharm"        -d "%REPO%\xenharm_service" cmd /k "%~dp0start-xenharm.bat" ^
^;      new-tab --title "supercollider"  -d "%REPO%\scripts"         cmd /k "%~dp0_run-all-supercollider.bat" ^
^;      focus-tab -t 0
    exit /b 0
)

REM --- fallback: three separate cmd windows ---
echo Windows Terminal (wt.exe) not found via PATH, %%LOCALAPPDATA%%\Microsoft\WindowsApps,
echo %%ProgramFiles%%\WindowsApps, or PowerShell Get-Command. Falling back to
echo three separate cmd windows. Install Windows Terminal from the Microsoft
echo Store for the nicer single-window-three-tabs layout.
echo.
start "xenharm"       cmd /k "%~dp0start-xenharm.bat"
start "xentool"       cmd /k "%~dp0_run-all-xentool.bat"
start "supercollider" cmd /k "%~dp0_run-all-supercollider.bat"
exit /b 0
