@echo off
REM Internal helper for run-all-{exquis,wooting}.bat. Launches the
REM xentool web editor for the layout passed in %1. Independent of
REM xentool serve / xenharm / SC — the editor is its own HTTP server
REM that reads and writes the .xtn / .wtn / .ltn file directly.
xentool edit %1
