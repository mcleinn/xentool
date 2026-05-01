@echo off
rem Run `xentool serve` against a Wooting layout.
rem
rem Usage:
rem   scripts\serve-wooting.bat                  (default: wtn\edo53.wtn)
rem   scripts\serve-wooting.bat wtn\edo24.wtn
rem   scripts\serve-wooting.bat wtn\edo31.wtn --output "loopMIDI Port"
rem
rem All arguments are forwarded to `xentool serve` verbatim. The script `cd`s
rem into the project root so relative paths like `wtn\foo.wtn` resolve.
rem
rem Requires the Wooting Analog and RGB SDKs to be installed
rem (scripts\install-wooting-sdks.ps1).

setlocal
set "SCRIPT_DIR=%~dp0"
set "PROJECT_ROOT=%SCRIPT_DIR%.."

pushd "%PROJECT_ROOT%" || exit /b 1

if "%~1"=="" (
    xentool serve wtn\edo53.wtn
) else (
    xentool serve %*
)
set "EXITCODE=%ERRORLEVEL%"
popd
endlocal & exit /b %EXITCODE%
