@echo off
setlocal

set "SCRIPT_DIR=%~dp0"

if exist "%SCRIPT_DIR%server.py" (
    py -3.12 "%SCRIPT_DIR%server.py" %*
) else (
    echo Could not find server.py next to this script.
    exit /b 1
)
