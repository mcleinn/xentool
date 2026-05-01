@echo off
rem Run `xentool serve` against an Exquis layout.
rem
rem Usage:
rem   scripts\serve-exquis.bat                   (default: xtn\edo53.xtn)
rem   scripts\serve-exquis.bat xtn\edo24.xtn
rem   scripts\serve-exquis.bat xtn\edo31.xtn --mts-esp
rem   scripts\serve-exquis.bat xtn\edo31.xtn --pb-range 48
rem
rem All arguments are forwarded to `xentool serve` verbatim. The script `cd`s
rem into the project root so relative paths like `xtn\foo.xtn` resolve.

setlocal
set "SCRIPT_DIR=%~dp0"
set "PROJECT_ROOT=%SCRIPT_DIR%.."

pushd "%PROJECT_ROOT%" || exit /b 1

if "%~1"=="" (
    xentool serve xtn\edo53.xtn
) else (
    xentool serve %*
)
set "EXITCODE=%ERRORLEVEL%"
popd
endlocal & exit /b %EXITCODE%
