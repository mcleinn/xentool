@echo off
REM Start the xenharm sidecar (microtonal note + interval glyphs for the
REM Live HUD). Listens on http://127.0.0.1:3199. Requires xenharmlib in
REM the active Python; see xenharm_service\README.md for venv setup.
cd /d "%~dp0..\xenharm_service"
python server.py
