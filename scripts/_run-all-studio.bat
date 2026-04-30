@echo off
REM Internal helper for run-all-{exquis,wooting}.bat. Launches the
REM tanpura_studio Flask relay (port 9100) that serves the touchscreen
REM UI and bridges HTTP -> OSC to the SuperCollider tanpura on 57121.
REM Waits ~6 s so SC's openUDPPort(57121) has run; the startup batch
REM push of the user default is then routed to the live synth on the
REM first sounding voice.
timeout /t 6 /nobreak >nul
cd /d "%~dp0..\supercollider\tanpura_studio"
python server.py
