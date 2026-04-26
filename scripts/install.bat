@echo off
rem Build and install xentool to %USERPROFILE%\.cargo\bin\.
rem
rem Usage: scripts\install.bat
rem
rem Thin wrapper around `cargo install --path . --force`.
rem Wooting SDKs are installed separately via install-wooting-sdks.ps1.

setlocal
set "SCRIPT_DIR=%~dp0"
set "PROJECT_ROOT=%SCRIPT_DIR%.."

pushd "%PROJECT_ROOT%" || (
    echo Failed to cd into %PROJECT_ROOT%
    pause
    exit /b 1
)

echo [install] Building and installing xentool from %PROJECT_ROOT%
cargo install --path . --force
if errorlevel 1 (
    popd
    echo [error] cargo install failed.
    pause
    exit /b 1
)
popd

echo.
echo [ok] Installed to %USERPROFILE%\.cargo\bin\xentool.exe
echo.
echo Open a fresh cmd / PowerShell, then verify with:  xentool --version
echo.
echo [note] For Wooting backend support, also run:
echo        powershell -ExecutionPolicy Bypass -File "%SCRIPT_DIR%install-wooting-sdks.ps1"
echo.
pause
endlocal
