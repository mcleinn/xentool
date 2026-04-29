@echo off
REM Internal helper called from `run-all-exquis.bat` / `run-all-wooting.bat`.
REM Expects the caller to have set:
REM
REM   LAYOUT      Absolute path to the .xtn / .wtn layout file (passed to
REM               xentool serve as the first positional argument).
REM   BACKEND     Cosmetic label for the wt window/tab title (`exquis` or
REM               `wooting`).
REM   SC_SCRIPT   Basename of the SuperCollider patch inside
REM               `supercollider/` (e.g. `mpe_tanpura_xentool.scd` for the
REM               Exquis MPE flow, `midi_piano_xentool.scd` for the
REM               Wooting classic-MIDI flow). Forwarded to
REM               start-supercollider.bat.
REM
REM Falls back to three separate cmd windows if Windows Terminal can't be
REM located. `-w new` always opens a fresh wt window so any existing wt
REM session is left untouched.

setlocal EnableDelayedExpansion
set "REPO=%~dp0.."
if not defined LAYOUT    set "LAYOUT="
if not defined BACKEND   set "BACKEND=xentool"
if not defined SC_SCRIPT set "SC_SCRIPT=mpe_tanpura_xentool.scd"

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
    echo Layout:                 !LAYOUT!
    echo SC patch:               !SC_SCRIPT!
    REM `;` separates tabs in the wt command — escape as `^;` for cmd.exe.
    REM `-w new` opens a fresh wt window; `focus-tab -t 0` focuses xentool.
    "!WT!" -w new ^
        new-tab --title "xentool (!BACKEND!)" -d "%REPO%"                 cmd /k "%~dp0_run-all-xentool.bat" "!LAYOUT!" ^
^;      new-tab --title "xenharm"             -d "%REPO%\xenharm_service" cmd /k "%~dp0start-xenharm.bat" ^
^;      new-tab --title "supercollider"       -d "%REPO%\scripts"         cmd /k "%~dp0_run-all-supercollider.bat" "!SC_SCRIPT!" ^
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
start "xentool"       cmd /k "%~dp0_run-all-xentool.bat" "!LAYOUT!"
start "supercollider" cmd /k "%~dp0_run-all-supercollider.bat" "!SC_SCRIPT!"
exit /b 0
