@echo off
REM Launch SuperCollider with one of the bundled patches. The patch
REM filename is taken from the first argument (basename inside
REM `supercollider/`); defaults to `mpe_tanpura_xentool.scd` so
REM running this script directly behaves the same as before.
REM
REM Usage:
REM   start-supercollider.bat                            (mpe_tanpura, default)
REM   start-supercollider.bat midi_piano_xentool.scd     (Wooting / classic MIDI)
REM
REM Resolves sclang from PATH first, then from the standard
REM "Program Files\SuperCollider-*" install layout.
setlocal

set "SCRIPT_NAME=%~1"
if "%SCRIPT_NAME%"=="" set "SCRIPT_NAME=mpe_tanpura_xentool.scd"
set "SCRIPT=%~dp0..\supercollider\%SCRIPT_NAME%"

if not exist "%SCRIPT%" (
    echo SuperCollider patch not found: %SCRIPT%
    exit /b 1
)

where sclang >nul 2>&1
if %ERRORLEVEL% EQU 0 (
    sclang "%SCRIPT%"
    exit /b %ERRORLEVEL%
)

set "SCLANG="
for /d %%D in ("%ProgramFiles%\SuperCollider-*") do (
    if exist "%%~fD\sclang.exe" set "SCLANG=%%~fD\sclang.exe"
)

if defined SCLANG (
    "%SCLANG%" "%SCRIPT%"
    exit /b %ERRORLEVEL%
)

echo Could not find sclang.exe. Add SuperCollider to PATH or edit this script.
exit /b 1
