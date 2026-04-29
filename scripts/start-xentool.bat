@echo off
REM Start xentool with the Live HUD enabled. Resumes the last-used layout
REM from settings.json by default. Edit the line below or pass a layout
REM as the first argument if you want to pin a specific .xtn / .wtn file.
REM
REM Used directly (double-click or `start-xentool.bat`) and indirectly by
REM `run-all-win11.bat` (which prepends a 2 s wait so xenharm has bound
REM first).
xentool serve %1 --hud
