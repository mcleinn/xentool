@echo off
REM Start xentool with the Live HUD enabled. Resumes the last-used layout
REM from settings.json by default. Edit the line below or pass a layout
REM as the first argument if you want to pin a specific .xtn / .wtn file.
REM
REM Used directly (double-click or `start-xentool.bat`) and indirectly by
REM `run-all-{exquis,wooting}.bat` (which prepend a 2 s wait so xenharm
REM has bound first, and may set XENTOOL_EXTRA_ARGS to add backend-
REM specific flags such as `--tune-supercollider` for the Wooting flow).
xentool serve %1 --hud %XENTOOL_EXTRA_ARGS%
