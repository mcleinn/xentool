@echo off
REM Launch SuperCollider with the bundled tanpura patch (foreground; this
REM window stays attached to sclang's stdout). Resolves sclang from PATH
REM first, then falls back to the standard "Program Files\SuperCollider-*"
REM install layout. Adapted from the original launcher in
REM C:\Dev-Free\SuperCollider\mpe_tanpura_xentool_start.bat.
setlocal

set "SCRIPT=%~dp0..\supercollider\mpe_tanpura_xentool.scd"

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
