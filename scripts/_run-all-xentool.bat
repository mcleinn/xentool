@echo off
REM Internal helper for run-all-win11.bat. Waits ~2 s so xenharm has time
REM to bind on 127.0.0.1:3199 before xentool's HUD probe fires (the probe
REM is sticky for the session — if it fires too early, the HUD has no
REM note glyphs until xentool is restarted). Then starts xentool.
timeout /t 2 /nobreak >nul
call "%~dp0start-xentool.bat"
