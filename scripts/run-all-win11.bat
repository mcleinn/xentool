@echo off
REM Open Windows Terminal with three tabs running xentool, xenharm and
REM SuperCollider in one window. Requires Windows 11 / Windows Terminal
REM (`wt.exe` on PATH).
REM
REM Tab order — leftmost first:
REM   1. xentool        (focused tab; waits 2 s so xenharm has bound)
REM   2. xenharm        (starts immediately)
REM   3. supercollider  (waits 5 s so xentool MIDI + loopMIDI are up)
REM
REM Each tab uses `cmd /k` so the process stays attached and you can read
REM its log; close the tab or Ctrl+C to stop that subsystem.
REM
REM Adjust the xentool layout argument below if you want a specific .xtn
REM file; without an argument xentool resumes the last-used layout from
REM settings.json.

setlocal
set "REPO=%~dp0.."

REM `;` separates tabs in the wt command; escape as `^;` for cmd.exe.
REM `focus-tab -t 0` brings the leftmost (xentool) tab to focus after
REM all three are open.
wt -w 0 ^
   new-tab --title "xentool"        -d "%REPO%"               cmd /k "timeout /t 2 /nobreak >nul && xentool serve --hud" ^
^; new-tab --title "xenharm"        -d "%REPO%\xenharm_service" cmd /k "python server.py" ^
^; new-tab --title "supercollider"  -d "%REPO%\scripts"         cmd /k "timeout /t 5 /nobreak >nul && start-supercollider.bat" ^
^; focus-tab -t 0
