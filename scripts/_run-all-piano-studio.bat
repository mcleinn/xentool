@echo off
REM Internal helper for run-all-wooting.bat. Launches the piano_studio
REM Flask relay (port 9101) that serves the touchscreen UI and bridges
REM HTTP -> OSC to the SuperCollider piano on 57123. Waits ~6 s so SC's
REM openUDPPort(57123) has run before any startup user-default push.
timeout /t 6 /nobreak >nul
cd /d "%~dp0..\supercollider\piano_studio"
python server.py
