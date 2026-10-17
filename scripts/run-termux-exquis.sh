#!/data/data/com.termux/files/usr/bin/bash
# xentool on Android / Termux (Exquis flow). See docs/android_termux.md.
#
# Opens a tmux session with four windows:
#   jack     — jackd on the software-timed `dummy` backend (scsynth's clock only)
#   bridge   — jack2pulse → pacat: JACK audio into a PulseAudio AAudio sink
#              (jack2's own opensles backend crackles at every period size;
#              PulseAudio's default OpenSL sink is clean but adds ~250 ms)
#   sc       — sclang running supercollider/mpe_tanpura_xentool.scd
#   xentool  — `xentool serve <layout> --hud --no-tui`, re-executed under
#              termux-usb automatically (accept the USB permission dialog once
#              per plug-in). XENTOOL_TUI=1 brings the terminal UI back.
#
# Prereqs (one-off):
#   pkg install rust git clang pkg-config libusb termux-api supercollider jack2 pulseaudio tmux
#   cargo build --release
#   The Termux:API companion app must be installed (termux-usb needs it).
#   jack2pulse is built from scripts/termux/jack2pulse.c on first run.
#
# Usage: scripts/run-termux-exquis.sh [layout.xtn]   (default xtn/edo31.xtn)
# Env:   JACK_RATE (48000 = AAudio sink rate, avoids resampling)
#        JACK_PERIOD (512)   PULSE_LATENCY_MS (20)
#        PULSE_SINK  (xentool_aaudio; set to OpenSL_ES_sink to use the default
#                     sink instead, at a much higher latency)
#        XENTOOL_TUI (0; 1 = run the ratatui TUI, costs ~30 % of a core)
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LAYOUT="${1:-xtn/edo31.xtn}"
SESSION="xentool"
JACK_RATE="${JACK_RATE:-48000}"
JACK_PERIOD="${JACK_PERIOD:-512}"
# The dummy backend's -w is its cycle time; keep it exactly one period.
JACK_WAIT_US=$(( JACK_PERIOD * 1000000 / JACK_RATE ))
PULSE_LATENCY_MS="${PULSE_LATENCY_MS:-20}"
PULSE_SINK="${PULSE_SINK:-xentool_aaudio}"
XENTOOL_TUI="${XENTOOL_TUI:-0}"
TUI_FLAG="--no-tui"; [ "$XENTOOL_TUI" = "1" ] && TUI_FLAG=""
FIFO="$HOME/.cache/xentool/jack2pulse.fifo"
BRIDGE="$PREFIX/bin/jack2pulse"

for c in jackd sclang tmux termux-usb pacat clang; do
    command -v "$c" >/dev/null 2>&1 || {
        echo "missing '$c' — run: pkg install jack2 supercollider tmux termux-api pulseaudio clang" >&2
        exit 1
    }
done

BIN="$ROOT/target/release/xentool"
[ -x "$BIN" ] || BIN="$ROOT/target/debug/xentool"
[ -x "$BIN" ] || { echo "xentool not built — run: cargo build --release (in $ROOT)" >&2; exit 1; }

if [ ! -x "$BRIDGE" ] || [ "$ROOT/scripts/termux/jack2pulse.c" -nt "$BRIDGE" ]; then
    echo "building jack2pulse..."
    clang -O2 -Wall -o "$BRIDGE" "$ROOT/scripts/termux/jack2pulse.c" -ljack
fi

if tmux has-session -t "$SESSION" 2>/dev/null; then
    exec tmux attach -t "$SESSION"
fi

# PulseAudio is normally already running (Termux starts it on demand);
# make sure, without touching its configuration.
pulseaudio --check 2>/dev/null || pulseaudio --start 2>/dev/null || true
# Low-latency output: load an extra AAudio sink at runtime (no config file
# is changed; the default OpenSL sink and source stay as they are, so TTS /
# microphone users are unaffected). It disappears when PulseAudio restarts.
if [ "$PULSE_SINK" = "xentool_aaudio" ] && ! pactl list short sinks | grep -q "xentool_aaudio"; then
    pactl load-module module-aaudio-sink sink_name=xentool_aaudio rate="$JACK_RATE" >/dev/null         || { echo "could not load module-aaudio-sink; falling back to the default sink" >&2; PULSE_SINK="@DEFAULT_SINK@"; }
fi

mkdir -p "$(dirname "$FIFO")"
[ -p "$FIFO" ] || { rm -f "$FIFO"; mkfifo "$FIFO"; }

if pgrep -x jackd >/dev/null 2>&1; then
    tmux new-session -d -s "$SESSION" -n jack "echo 'jackd already running'; sleep infinity"
else
    tmux new-session -d -s "$SESSION" -n jack \
        "jackd -d dummy -r $JACK_RATE -p $JACK_PERIOD -w $JACK_WAIT_US"
    sleep 2
fi
# pacat must get the FIFO as a *file argument*: reading stdin makes it derive
# a media name through the locale, which fails on Termux ("Failed to set
# media name"). jack2pulse writes into the FIFO.
tmux new-window -t "$SESSION" -n bridge \
    "pkill -x jack2pulse 2>/dev/null; pacat --raw --format=s16le --rate=$JACK_RATE --channels=2 --latency-msec=$PULSE_LATENCY_MS --device='$PULSE_SINK' '$FIFO' & '$BRIDGE' > '$FIFO'; echo '[bridge exited — press Enter]'; read -r _"
sleep 1
tmux new-window -t "$SESSION" -n sc \
    "cd '$ROOT/supercollider' && sclang mpe_tanpura_xentool.scd"
# Give scsynth a moment to boot before xentool starts streaming OSC.
sleep 3
tmux new-window -t "$SESSION" -n xentool \
    "cd '$ROOT' && '$BIN' serve '$LAYOUT' --hud $TUI_FLAG; echo; echo '[xentool exited — press Enter]'; read -r _"
tmux select-window -t "$SESSION:xentool"
exec tmux attach -t "$SESSION"
