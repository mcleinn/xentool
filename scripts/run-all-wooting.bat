@echo off
REM Launch the full xentool stack for the Wooting backend in one Windows
REM Terminal window: xentool serve wtn\edo31.wtn (with --hud) on the
REM left tab, xenharm sidecar in the middle, SuperCollider tanpura on
REM the right. To use a different .wtn, set LAYOUT before calling.
REM
REM Falls back to three separate cmd windows if Windows Terminal isn't
REM available. Always opens a NEW wt window — your existing terminals
REM are untouched.

setlocal
set "BACKEND=wooting"
if not defined LAYOUT set "LAYOUT=%~dp0..\wtn\edo31.wtn"
call "%~dp0_run-all-common.bat"
