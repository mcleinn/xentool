@echo off
REM Internal helper for run-all-{exquis,wooting}.bat. Waits ~5 s so
REM xentool's MIDI input is up before SuperCollider's MIDIClient.init
REM enumerates ports (and so SC's startup OSC sends to xentool's HUD
REM don't fire before xentool's UDP listener is bound — avoids Windows
REM ICMP noise). The first argument is the patch basename inside
REM `supercollider/` (forwarded to start-supercollider.bat); defaults to
REM `mpe_tanpura_xentool.scd`.
timeout /t 5 /nobreak >nul
call "%~dp0start-supercollider.bat" %1
