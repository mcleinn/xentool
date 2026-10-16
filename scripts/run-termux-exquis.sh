#!/data/data/com.termux/files/usr/bin/bash
# xentool on Android / Termux (Exquis flow).
#
# Opens a tmux session with three windows:
#   jack     — jackd on the OpenSL ES backend (scsynth needs JACK on Linux builds)
#   sc       — sclang running supercollider/mpe_tanpura_xentool.scd
#   xentool  — `xentool serve <layout> --hud`, re-executed under termux-usb
#              automatically (accept the USB permission dialog once per plug-in)
#
# Prereqs (one-off):
#   pkg install rust git clang pkg-config libusb termux-api supercollider jack2 tmux
#   cargo build --release
#   The Termux:API companion app must be installed (termux-usb needs it).
#
# Usage: scripts/run-termux-exquis.sh [layout.xtn]   (default xtn/edo31.xtn)
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LAYOUT="${1:-xtn/edo31.xtn}"
SESSION="xentool"

for c in jackd sclang tmux termux-usb; do
    command -v "$c" >/dev/null 2>&1 || {
        echo "missing '$c' — run: pkg install jack2 supercollider tmux termux-api" >&2
        exit 1
    }
done

BIN="$ROOT/target/release/xentool"
[ -x "$BIN" ] || BIN="$ROOT/target/debug/xentool"
[ -x "$BIN" ] || { echo "xentool not built — run: cargo build --release (in $ROOT)" >&2; exit 1; }

if tmux has-session -t "$SESSION" 2>/dev/null; then
    exec tmux attach -t "$SESSION"
fi

if pgrep -x jackd >/dev/null 2>&1; then
    tmux new-session -d -s "$SESSION" -n jack "echo 'jackd already running'; sleep infinity"
else
    tmux new-session -d -s "$SESSION" -n jack "jackd -d opensles"
    sleep 2
fi
tmux new-window -t "$SESSION" -n sc \
    "cd '$ROOT/supercollider' && sclang mpe_tanpura_xentool.scd"
# Give scsynth a moment to boot before xentool starts streaming OSC.
sleep 3
tmux new-window -t "$SESSION" -n xentool \
    "cd '$ROOT' && '$BIN' serve '$LAYOUT' --hud; echo; echo '[xentool exited — press Enter]'; read -r _"
tmux select-window -t "$SESSION:xentool"
exec tmux attach -t "$SESSION"
