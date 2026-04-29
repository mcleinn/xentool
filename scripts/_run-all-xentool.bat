@echo off
REM Internal helper for run-all-{exquis,wooting}.bat. Waits ~2 s so
REM xenharm has time to bind on 127.0.0.1:3199 before xentool's HUD
REM probe fires (the probe is sticky for the session — if it fires
REM too early, the HUD has no note glyphs until xentool is restarted).
REM Then starts xentool with the layout passed in %1 (forwarded to
REM start-xentool.bat).
timeout /t 2 /nobreak >nul
call "%~dp0start-xentool.bat" %1
