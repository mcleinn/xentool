@echo off
REM Launch the full xentool stack for the Wooting backend in one Windows
REM Terminal window: xentool serve wtn\edo31.wtn (with --hud) on the
REM left tab, xenharm sidecar in the middle, the classic-MIDI piano
REM SuperCollider patch on the right (Wooting emits straight 12/N-EDO
REM MIDI, not MPE — so it uses midi_piano_xentool.scd, not the MPE
REM tanpura).
REM
REM Override defaults by setting LAYOUT or SC_SCRIPT before calling.
REM Falls back to three separate cmd windows if Windows Terminal isn't
REM available. Always opens a NEW wt window — your existing terminals
REM are untouched.

setlocal
set "BACKEND=wooting"
if not defined LAYOUT             set "LAYOUT=%~dp0..\wtn\edo53.wtn"
if not defined SC_SCRIPT          set "SC_SCRIPT=midi_piano_xentool.scd"
REM SC has no MTS-ESP client; xentool needs to push the active EDO and
REM pitch_offset over OSC so the SC patch can re-derive frequencies on
REM layout cycle. The Exquis flow doesn't need this — the Exquis backend
REM uses MPE pitch-bend retuning, so SC's `num.midicps` + the bend gives
REM the correct microtonal pitch.
if not defined XENTOOL_EXTRA_ARGS set "XENTOOL_EXTRA_ARGS=--tune-supercollider"
REM piano_studio Flask relay on http://localhost:9101 — touchscreen UI
REM that tweaks the piano SynthDef live via OSC on 57123.
if not defined STUDIO_SCRIPT      set "STUDIO_SCRIPT=_run-all-piano-studio.bat"
if not defined STUDIO_TITLE       set "STUDIO_TITLE=piano studio"
if not defined STUDIO_DIR         set "STUDIO_DIR=%~dp0..\supercollider\piano_studio"
if not defined EDIT_SCRIPT        set "EDIT_SCRIPT=_run-all-edit.bat"
if not defined EDIT_TITLE         set "EDIT_TITLE=xentool edit"
call "%~dp0_run-all-common.bat"
