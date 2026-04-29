@echo off
REM Internal helper for run-all-win11.bat. Waits ~5 s so xentool's MIDI
REM input is up before SuperCollider's MIDIClient.init enumerates ports
REM (and so SC's startup OSC sends to xentool's HUD don't fire before
REM xentool's UDP listener is bound — avoids Windows ICMP noise).
timeout /t 5 /nobreak >nul
call "%~dp0start-supercollider.bat"
