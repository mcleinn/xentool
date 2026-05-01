@echo off
REM Launch the full xentool stack for the Exquis backend in one Windows
REM Terminal window: xentool serve xtn\edo31.xtn (with --hud) on the
REM left tab, xenharm sidecar in the middle, SuperCollider tanpura on
REM the right. To use a different .xtn, set LAYOUT before calling.
REM
REM Falls back to three separate cmd windows if Windows Terminal isn't
REM available. Always opens a NEW wt window — your existing terminals
REM are untouched.

setlocal
set "BACKEND=exquis"
if not defined LAYOUT        set "LAYOUT=%~dp0..\xtn\edo53.xtn"
if not defined SC_SCRIPT     set "SC_SCRIPT=mpe_tanpura_xentool.scd"
if not defined STUDIO_SCRIPT set "STUDIO_SCRIPT=_run-all-studio.bat"
if not defined STUDIO_TITLE  set "STUDIO_TITLE=tanpura studio"
if not defined STUDIO_DIR    set "STUDIO_DIR=%~dp0..\supercollider\tanpura_studio"
if not defined EDIT_SCRIPT   set "EDIT_SCRIPT=_run-all-edit.bat"
if not defined EDIT_TITLE    set "EDIT_TITLE=xentool edit"
call "%~dp0_run-all-common.bat"
