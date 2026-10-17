#!/data/data/com.termux/files/usr/bin/bash
# Android / Termux installer for the Exquis backend of xentool.
#
# Counterpart of scripts/install-linux-exquis.sh for a phone running Termux
# (tested: Pixel 7, Android 14, Termux + Termux:API from Google Play).
# Installs the Termux packages, builds xentool (release) and the
# jack2pulse audio bridge, and checks that termux-usb can talk to the
# Termux:API app. No services, no autostart — run
# scripts/run-termux-exquis.sh afterwards. See docs/android_termux.md.
#
# Usage:
#   bash scripts/install-termux-exquis.sh
#
# It never runs `pkg upgrade` and never touches PulseAudio's config, so an
# existing TTS / microphone setup on the phone keeps working.

set -euo pipefail

step() { printf '\033[36m[install]\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m[ok]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
err()  { printf '\033[31m[error]\033[0m %s\n' "$*" >&2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ -z "${PREFIX:-}" || "$PREFIX" != *com.termux* ]]; then
    err "This script targets Termux on Android (PREFIX=$PREFIX). Use install-linux-*.sh or install.bat elsewhere."
    exit 1
fi

# ---------- packages ----------
# rust/clang/pkg-config/libusb: build xentool (rusb links the system libusb).
# termux-api: termux-usb (device access), termux-clipboard-set, termux-open-url.
# supercollider + jack2: scsynth needs JACK on Linux-style builds.
# pulseaudio: the AAudio sink the audio bridge plays into. tmux: the session.
PKGS=(git rust clang pkg-config libusb termux-api supercollider jack2 pulseaudio tmux)
missing=()
for p in "${PKGS[@]}"; do
    if dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed'; then
        ok "  $p (already installed)"
    else
        missing+=("$p")
    fi
done
if [[ ${#missing[@]} -gt 0 ]]; then
    step "pkg install ${missing[*]}"
    pkg install -y "${missing[@]}"
fi

# ---------- Termux:API companion app ----------
step "Checking termux-usb (needs the Termux:API app installed and allowed to run)"
if usb_list="$(timeout 15 termux-usb -l 2>&1)"; then
    ok "  termux-usb works; devices now: $(echo "$usb_list" | tr -d '\n ')"
else
    warn "  termux-usb -l failed or timed out. Install the Termux:API app (same source as Termux)"
    warn "  and allow it to run in the background; xentool cannot open the Exquis without it."
fi

# ---------- build ----------
step "Building xentool (cargo build --release in $ROOT)"
(cd "$ROOT" && cargo build --release)
ok "  $ROOT/target/release/xentool"

step "Building the JACK → PulseAudio bridge (scripts/termux/jack2pulse.c)"
clang -O2 -Wall -o "$PREFIX/bin/jack2pulse" "$ROOT/scripts/termux/jack2pulse.c" -ljack
ok "  $PREFIX/bin/jack2pulse"

# ---------- convenience ----------
mkdir -p "$HOME/.cache/xentool" "$HOME/.local/bin"
ln -sf "$ROOT/target/release/xentool" "$HOME/.local/bin/xentool"
ln -sf "$ROOT/scripts/run-termux-exquis.sh" "$HOME/.local/bin/xentool-exquis"
# Also a symlink right in the Termux home: `~/xentool-exquis` starts
# SuperCollider and then serves the tuning (default xtn/edo53.xtn).
ln -sf "$ROOT/scripts/run-termux-exquis.sh" "$HOME/xentool-exquis"
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) warn "  add 'export PATH=\$HOME/.local/bin:\$PATH' to ~/.bashrc to use 'xentool' and 'xentool-exquis' directly" ;;
esac
ok "  ~/xentool-exquis, ~/.local/bin/xentool-exquis (launcher) and ~/.local/bin/xentool"

step "Smoke test"
"$ROOT/target/release/xentool" help >/dev/null && ok "  xentool help"
if pulseaudio --check 2>/dev/null; then ok "  PulseAudio running"; else warn "  PulseAudio not running; the launcher starts it"; fi

cat <<EOF

Done. Plug the Exquis into USB-C, then:

    ~/xentool-exquis                 # xtn/edo53.xtn
    ~/xentool-exquis xtn/edo31.xtn

Accept the Android USB permission dialog on first use after each plug-in.
The launcher opens a tmux session (jackd, audio bridge, sclang, xentool);
detach with Ctrl-b d, re-attach by running it again.
EOF
