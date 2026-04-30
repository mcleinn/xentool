#!/usr/bin/env bash
# Shared helpers for `install-linux-exquis.sh` and `install-linux-wooting.sh`.
#
# This file is *sourced* — it never executes on its own. The wrappers set
# `BACKEND` (`exquis` | `wooting`) and `DEFAULT_LAYOUT` before calling
# `install_linux_main`.
#
# Targets: Patchbox OS / Raspberry Pi OS / Ubuntu on Pi 4/5. Anywhere else
# with apt should also work; non-apt distros are flagged but not auto-handled.

set -euo pipefail

# ---------- pretty output ----------

step() { printf '\033[36m[install]\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m[ok]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
err()  { printf '\033[31m[error]\033[0m %s\n' "$*" >&2; }
hr()   { printf '\033[2m%s\033[0m\n' "------------------------------------------------------------"; }

yesno() {
    # yesno "Question?" "Y" → defaults to yes. "N" → defaults to no.
    local prompt="$1" default="${2:-Y}" answer
    local hint="[y/N]"
    [[ "$default" =~ ^[Yy]$ ]] && hint="[Y/n]"
    while true; do
        read -r -p "$prompt $hint " answer || answer=""
        answer="${answer:-$default}"
        case "$answer" in
            [Yy]|[Yy][Ee][Ss]) return 0 ;;
            [Nn]|[Nn][Oo])     return 1 ;;
            *) echo "  please answer y or n" ;;
        esac
    done
}

# ---------- distro / preconditions ----------

require_linux() {
    if [[ "$(uname -s)" != "Linux" ]]; then
        err "This script targets Linux. On Windows use scripts\\install.bat; on macOS, build manually with cargo install --path ."
        exit 1
    fi
}

detect_pkg_mgr() {
    if   command -v apt-get >/dev/null 2>&1; then echo apt
    elif command -v dnf     >/dev/null 2>&1; then echo dnf
    elif command -v pacman  >/dev/null 2>&1; then echo pacman
    else                                        echo unknown
    fi
}

apt_install_missing() {
    # apt_install_missing pkg1 pkg2 ... — installs only those not already
    # present, prints which were already there.
    local missing=()
    for p in "$@"; do
        if dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed'; then
            ok "  $p (already installed)"
        else
            missing+=("$p")
        fi
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        step "apt install ${missing[*]}"
        sudo apt-get update -q
        sudo apt-get install -y "${missing[@]}"
    fi
}

# ---------- toolchain ----------

ensure_rust() {
    if command -v cargo >/dev/null 2>&1; then
        ok "Rust toolchain present ($(cargo --version))"
        return
    fi
    step "Rust toolchain missing — installing rustup"
    if ! yesno "  Install Rust via rustup (https://rustup.rs)?" Y; then
        err "Rust is required to build xentool. Aborting."
        exit 1
    fi
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
}

build_xentool() {
    step "Building xentool (cargo install --path .)"
    pushd "$REPO_ROOT" >/dev/null
    cargo install --path . --force
    popd >/dev/null
    ok "xentool installed at $HOME/.cargo/bin/xentool"
}

# ---------- Wooting SDKs ----------

# Install both the Wooting Analog SDK and the Wooting RGB SDK from upstream
# release assets. Idempotent — skips either step if its `.so` is already in
# place. Inlined from the former scripts/install-wooting-sdks.sh.
install_wooting_sdks() {
    step "Installing Wooting Analog + RGB SDKs"
    apt_install_missing curl unzip tar

    local arch work
    arch="$(uname -m)"
    work="${TMPDIR:-/tmp}/xentool-wooting-install"
    mkdir -p "$work"

    gh_latest_asset_url() {
        local repo="$1" regex="$2"
        curl -sSL "https://api.github.com/repos/$repo/releases/latest" \
          | grep -oE '"browser_download_url":\s*"[^"]+"' \
          | sed -E 's/.*"(https[^"]+)".*/\1/' \
          | grep -E "$regex" | head -n1
    }

    # Analog SDK ---------------------------------------------------------------
    if [[ -f /usr/lib/x86_64-linux-gnu/libwooting_analog_sdk.so ]] \
       || [[ -f /usr/local/lib/libwooting_analog_sdk.so ]]; then
        ok "  Wooting Analog SDK already installed"
    else
        if command -v dpkg >/dev/null 2>&1 && [[ "$arch" == "x86_64" ]]; then
            local url deb
            url="$(gh_latest_asset_url WootingKb/wooting-analog-sdk '\.deb$')"
            deb="$work/$(basename "$url")"
            step "  Analog SDK: downloading $url"
            curl -sSL -o "$deb" "$url"
            sudo dpkg -i "$deb" || sudo apt-get install -f -y
            ok "  Wooting Analog SDK installed (.deb)"
        else
            local regex url tgz so
            case "$arch" in
                x86_64)  regex='x86_64-unknown-linux-gnu\.tar\.gz$' ;;
                aarch64) regex='aarch64-.*linux.*\.tar\.gz$' ;;
                *) err "Unsupported arch for Wooting Analog SDK: $arch"; return 1 ;;
            esac
            url="$(gh_latest_asset_url WootingKb/wooting-analog-sdk "$regex")"
            [[ -n "$url" ]] || { err "  No Analog SDK asset matched $regex"; return 1; }
            tgz="$work/$(basename "$url")"
            step "  Analog SDK: downloading $url"
            curl -sSL -o "$tgz" "$url"
            mkdir -p "$work/analog"
            tar -xzf "$tgz" -C "$work/analog"
            so="$(find "$work/analog" -name 'libwooting_analog_sdk*.so*' -print -quit)"
            [[ -n "$so" ]] || { err "  libwooting_analog_sdk.so not found in archive"; return 1; }
            sudo install -D "$so" /usr/local/lib/libwooting_analog_sdk.so
            sudo ldconfig
            ok "  Wooting Analog SDK installed to /usr/local/lib"
        fi
    fi

    # RGB SDK ------------------------------------------------------------------
    if [[ -f /usr/local/lib/libwooting-rgb-sdk.so ]] || [[ -f /usr/lib/libwooting-rgb-sdk.so ]]; then
        ok "  Wooting RGB SDK already installed"
    else
        local url zip so
        url="$(gh_latest_asset_url WootingKb/wooting-rgb-sdk 'ubuntu-x64\.zip$')"
        [[ -n "$url" ]] || { err "  No RGB SDK asset found"; return 1; }
        zip="$work/$(basename "$url")"
        step "  RGB SDK: downloading $url"
        curl -sSL -o "$zip" "$url"
        rm -rf "$work/rgb" && mkdir "$work/rgb"
        unzip -q "$zip" -d "$work/rgb"
        so="$(find "$work/rgb" -name 'libwooting-rgb-sdk*.so*' -print -quit)"
        [[ -n "$so" ]] || { err "  libwooting-rgb-sdk.so not found in archive"; return 1; }
        sudo install -D "$so" /usr/local/lib/libwooting-rgb-sdk.so
        sudo ldconfig
        ok "  Wooting RGB SDK installed"
    fi
}

# ---------- python / xenharm ----------

# Picks the best available `python3.X` (preferring 3.12 down to 3.10) and
# echoes its absolute path. Returns non-zero if nothing in range is found.
detect_python() {
    local candidates=("python3.12" "python3.11" "python3.10")
    for c in "${candidates[@]}"; do
        if command -v "$c" >/dev/null 2>&1; then
            command -v "$c"
            return 0
        fi
    done
    # Fall back to default python3 if its version is 3.10+.
    if command -v python3 >/dev/null 2>&1; then
        local v
        v="$(python3 -c 'import sys; print("{}{}".format(sys.version_info.major, sys.version_info.minor))')"
        if [[ "$v" -ge 310 ]]; then
            command -v python3
            return 0
        fi
    fi
    return 1
}

setup_xenharm() {
    step "xenharm sidecar (microtonal note glyphs for the Live HUD)"
    if ! yesno "  Install xenharm? (requires Python 3.10+ and xenharmlib via pip)" Y; then
        warn "  Skipping xenharm. The HUD will fall back to numeric note labels."
        XENHARM_INSTALLED=0
        return
    fi

    XENHARM_PY="$(detect_python || true)"
    if [[ -z "${XENHARM_PY:-}" ]]; then
        warn "  No Python ≥3.10 found. Trying apt-install of python3.12 / 3.11."
        if [[ "$(detect_pkg_mgr)" == apt ]]; then
            sudo apt-get update -q
            sudo apt-get install -y python3.12 python3.12-venv python3-pip 2>/dev/null \
              || sudo apt-get install -y python3.11 python3.11-venv python3-pip 2>/dev/null \
              || true
        fi
        XENHARM_PY="$(detect_python || true)"
    fi
    if [[ -z "${XENHARM_PY:-}" ]]; then
        err "  Couldn't find or install a suitable Python (≥3.10). Skipping xenharm."
        XENHARM_INSTALLED=0
        return
    fi
    ok "  Using Python: $XENHARM_PY ($("$XENHARM_PY" --version 2>&1))"

    # Debian splits the venv module into a separate apt package (`pythonX.Y-venv`).
    # Without it, `python -m venv` fails with "ensurepip is not available".
    if [[ "$(detect_pkg_mgr)" == apt ]]; then
        local pyver
        pyver="$("$XENHARM_PY" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
        apt_install_missing "python${pyver}-venv" "python3-pip"
    fi

    # Per-user venv keeps xenharmlib out of the system Python and avoids
    # PEP-668 "externally-managed-environment" errors on Debian Bookworm.
    XENHARM_VENV="$HOME/.local/share/xentool/venv"
    if [[ ! -d "$XENHARM_VENV" ]]; then
        step "  Creating venv at $XENHARM_VENV"
        if ! "$XENHARM_PY" -m venv "$XENHARM_VENV"; then
            err "  venv creation failed. Try: sudo apt install python${pyver:-3}-venv"
            XENHARM_INSTALLED=0
            return
        fi
    fi
    step "  Installing xenharmlib into venv"
    "$XENHARM_VENV/bin/pip" install --upgrade pip >/dev/null
    "$XENHARM_VENV/bin/pip" install --upgrade xenharmlib >/dev/null
    ok "  xenharmlib installed ($("$XENHARM_VENV/bin/python" -c 'import xenharmlib; print(xenharmlib.__version__)' 2>/dev/null || echo unknown))"

    XENHARM_PY_BIN="$XENHARM_VENV/bin/python"
    XENHARM_INSTALLED=1
}

# ---------- supercollider ----------

setup_studio() {
    # Tanpura studio (Exquis backend) or piano studio (Wooting backend).
    # Both are Flask + python-osc relays that talk to the matching SC
    # patch over OSC. Reuses the xenharm venv (saves disk space and
    # avoids a second pip install dance); falls back to creating a
    # dedicated venv if xenharm wasn't installed.
    local studio_dir studio_label
    case "$BACKEND" in
        wooting) studio_dir="$REPO_ROOT/supercollider/piano_studio";   studio_label="piano studio (http://localhost:9101/)" ;;
        *)       studio_dir="$REPO_ROOT/supercollider/tanpura_studio"; studio_label="tanpura studio (http://localhost:9100/)" ;;
    esac
    step "Studio web UI: $studio_label"
    if [[ ! -f "$studio_dir/server.py" ]]; then
        warn "  $studio_dir/server.py missing — skipping."
        STUDIO_INSTALLED=0
        return
    fi
    if ! yesno "  Install + autostart the studio web UI?" Y; then
        warn "  Skipping studio. xentool + SC still work without it."
        STUDIO_INSTALLED=0
        return
    fi

    # Reuse xenharm's venv (created earlier in setup_xenharm). If absent,
    # create a small dedicated one — same Python detection logic.
    if [[ -n "${XENHARM_VENV:-}" && -x "$XENHARM_VENV/bin/python" ]]; then
        STUDIO_VENV="$XENHARM_VENV"
        ok "  Reusing xenharm venv at $STUDIO_VENV"
    else
        STUDIO_VENV="$HOME/.local/share/xentool/studio-venv"
        local studio_py
        studio_py="$(detect_python || true)"
        if [[ -z "$studio_py" ]]; then
            err "  No Python ≥3.10 found. Skipping studio."
            STUDIO_INSTALLED=0
            return
        fi
        if [[ ! -d "$STUDIO_VENV" ]]; then
            step "  Creating venv at $STUDIO_VENV"
            "$studio_py" -m venv "$STUDIO_VENV" || {
                err "  venv creation failed. Skipping studio."
                STUDIO_INSTALLED=0
                return
            }
        fi
    fi

    step "  Installing Flask + python-osc into $STUDIO_VENV"
    "$STUDIO_VENV/bin/pip" install --upgrade pip >/dev/null
    "$STUDIO_VENV/bin/pip" install -r "$studio_dir/requirements.txt" >/dev/null
    ok "  studio python deps ready"
    STUDIO_PY_BIN="$STUDIO_VENV/bin/python"
    STUDIO_DIR="$studio_dir"
    STUDIO_INSTALLED=1
}

setup_supercollider() {
    step "SuperCollider tanpura synth (mpe_tanpura_xentool.scd)"
    if ! yesno "  Install + autostart the SuperCollider tanpura synth?" N; then
        warn "  Skipping SuperCollider service. xentool MIDI still works without it."
        SC_INSTALLED=0
        return
    fi

    if [[ "$(detect_pkg_mgr)" == apt ]]; then
        apt_install_missing supercollider sc3-plugins
    elif ! command -v sclang >/dev/null 2>&1; then
        warn "  Install SuperCollider yourself, then re-run this script."
        SC_INSTALLED=0
        return
    fi
    SC_BIN="$(command -v sclang)"
    ok "  sclang: $SC_BIN"
    SC_INSTALLED=1
}

# ---------- systemd user units ----------

systemd_user_dir() {
    local d="$HOME/.config/systemd/user"
    mkdir -p "$d"
    echo "$d"
}

write_unit() {
    # write_unit <unit-name> <body-on-stdin>
    local name="$1"
    local path
    path="$(systemd_user_dir)/$name"
    cat > "$path"
    ok "  wrote $path"
}

write_xenharm_unit() {
    [[ "${XENHARM_INSTALLED:-0}" == 1 ]] || return
    write_unit xenharm.service <<UNIT
[Unit]
Description=XenHarm note name service (xentool sidecar)
After=network.target

[Service]
Type=simple
ExecStart=$XENHARM_PY_BIN $REPO_ROOT/xenharm_service/server.py --host 127.0.0.1 --port 3199
WorkingDirectory=$REPO_ROOT/xenharm_service
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
UNIT
}

write_xentool_unit() {
    # xentool's TUI needs a real pty (raw mode + alternate screen). Systemd
    # services don't have one, so we wrap in a detached tmux session under
    # a dedicated socket label `xentool`. Attach later from anywhere with:
    #
    #     tmux -L xentool attach -t xentool
    #
    # The session keeps running in the background between attaches; press
    # Ctrl-b d to detach without quitting xentool.
    local exec_args=("serve" "--hud" "--hud-port" "9099")
    if [[ -n "${XENTOOL_LAYOUT:-}" ]]; then
        exec_args=("serve" "$XENTOOL_LAYOUT" "--hud" "--hud-port" "9099")
    fi
    if [[ -n "${XENTOOL_MIDI_OUTPUT:-}" ]]; then
        exec_args+=("--output" "\"$XENTOOL_MIDI_OUTPUT\"")
    fi
    local cmd="$HOME/.cargo/bin/xentool ${exec_args[*]}"
    write_unit xentool.service <<UNIT
[Unit]
Description=xentool serve ($BACKEND backend, Live HUD on http://localhost:9099/)
After=network.target sound.target xenharm.service
Wants=xenharm.service

[Service]
# tmux fork-and-detach: succeed once the session is created, stay "active"
# (RemainAfterExit) so 'systemctl --user stop' can tear it down via ExecStop.
# If xentool itself crashes the tmux pane shows '[exited]'; recover with
# 'systemctl --user restart xentool.service'.
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/bin/tmux -L xentool new-session -d -s xentool '$cmd'
ExecStop=/usr/bin/tmux -L xentool kill-session -t xentool

[Install]
WantedBy=default.target
UNIT
}

# Small wrapper that any user can run from anywhere to bring up the TUI
# of the currently-running xentool service. Installed alongside the
# systemd units so it's discoverable from the post-install help.
write_xentool_tui_helper() {
    local bin="$HOME/.local/bin/xentool-tui"
    mkdir -p "$(dirname "$bin")"
    cat > "$bin" <<'WRAP'
#!/usr/bin/env bash
# Attach to the xentool TUI running under systemd. Detach with Ctrl-b d.
exec tmux -L xentool attach -t xentool
WRAP
    chmod +x "$bin"
    ok "  wrote $bin"
}

write_studio_unit() {
    [[ "${STUDIO_INSTALLED:-0}" == 1 ]] || return
    local title port
    case "$BACKEND" in
        wooting) title="piano studio";   port=9101 ;;
        *)       title="tanpura studio"; port=9100 ;;
    esac
    write_unit xentool-studio.service <<UNIT
[Unit]
Description=$title web UI for xentool ($BACKEND backend, http://localhost:$port/)
After=xentool-supercollider.service sound.target
Wants=xentool-supercollider.service

[Service]
Type=simple
ExecStart=$STUDIO_PY_BIN $STUDIO_DIR/server.py
WorkingDirectory=$STUDIO_DIR
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
UNIT
}

write_supercollider_unit() {
    [[ "${SC_INSTALLED:-0}" == 1 ]] || return
    # Exquis pads emit MPE; Wooting emits classic 12/N-EDO MIDI. The
    # bundled patches handle each:
    #   exquis  → mpe_tanpura_xentool.scd  (microtonal MPE tanpura)
    #   wooting → midi_piano_xentool.scd   (classic-MIDI piano)
    local sc_patch
    case "$BACKEND" in
        wooting) sc_patch="midi_piano_xentool.scd" ;;
        *)       sc_patch="mpe_tanpura_xentool.scd" ;;
    esac
    write_unit xentool-supercollider.service <<UNIT
[Unit]
Description=SuperCollider synth for xentool ($BACKEND → $sc_patch via ${XENTOOL_MIDI_OUTPUT:-Xentool ${BACKEND^}} virtual MIDI port)
After=xentool.service sound.target
Wants=xentool.service

[Service]
Type=simple
ExecStart=$SC_BIN $REPO_ROOT/supercollider/$sc_patch
WorkingDirectory=$REPO_ROOT/supercollider
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
UNIT
}

enable_lingering() {
    # Without lingering, user services stop on logout. Patchbox boots into
    # a dedicated pi user that's never interactively logged in, so
    # lingering is essential.
    if loginctl show-user "$USER" 2>/dev/null | grep -q 'Linger=yes'; then
        ok "  systemd-linger already enabled for $USER"
    elif yesno "  Enable systemd-linger for $USER (so services run without login)?" Y; then
        sudo loginctl enable-linger "$USER"
        ok "  Linger enabled"
    fi
}

start_services() {
    step "Reloading and starting services"
    systemctl --user daemon-reload

    # Order matters only at start time; systemd `After=` declares
    # dependencies but doesn't enforce sequencing on enable.
    local units=()
    [[ "${XENHARM_INSTALLED:-0}" == 1 ]] && units+=(xenharm.service)
    units+=(xentool.service)
    [[ "${SC_INSTALLED:-0}"      == 1 ]] && units+=(xentool-supercollider.service)
    [[ "${STUDIO_INSTALLED:-0}"  == 1 ]] && units+=(xentool-studio.service)

    for u in "${units[@]}"; do
        systemctl --user enable "$u"
        systemctl --user restart "$u" || warn "  $u failed to start; check journalctl --user -u $u"
    done

    sleep 1
    hr
    for u in "${units[@]}"; do
        systemctl --user --no-pager --lines=0 status "$u" || true
    done
}

# ---------- main ----------

print_plan() {
    hr
    echo "xentool install — $BACKEND backend"
    echo
    echo "  Repo:         $REPO_ROOT"
    echo "  Backend:      $BACKEND"
    echo "  Layout:       ${XENTOOL_LAYOUT:-<resume last>}"
    echo "  MIDI output:  ${XENTOOL_MIDI_OUTPUT:-<xentool default>}"
    echo
    echo "Steps:"
    echo "  1. Install required apt packages (build tools, ALSA dev headers, USB libs)"
    echo "  2. Ensure Rust toolchain"
    echo "  3. Build + install xentool to ~/.cargo/bin/"
    [[ "$BACKEND" == wooting ]] && echo "  4. Install Wooting Analog + RGB SDKs"
    echo "  5. (optional) xenharm sidecar — Python 3.10+ in a venv"
    echo "  6. (optional) SuperCollider tanpura synth"
    echo "  7. Write systemd user units, enable lingering, start services"
    echo
    hr
}

prompt_layout() {
    local kind="$1" dir="$2"
    if [[ ! -d "$dir" ]]; then
        XENTOOL_LAYOUT=""
        return
    fi
    echo
    echo "Available .$kind layouts in $dir:"
    local files=("$dir"/*."$kind")
    [[ -e "${files[0]}" ]] || { XENTOOL_LAYOUT=""; return; }
    local i=0
    for f in "${files[@]}"; do
        i=$((i + 1))
        printf "  %d) %s\n" "$i" "$(basename "$f")"
    done
    echo "  0) <resume last-used at runtime>"
    local sel
    read -r -p "Pick a default layout for the systemd service [0]: " sel
    sel="${sel:-0}"
    if [[ "$sel" -ge 1 && "$sel" -le ${#files[@]} ]]; then
        XENTOOL_LAYOUT="${files[$((sel - 1))]}"
    else
        XENTOOL_LAYOUT=""
    fi
}

# Verify the ALSA sequencer (snd-seq) is available. xentool's Linux MIDI
# output uses ALSA seq's virtual-port API to publish a "Xentool Wooting"
# (or "Xentool Exquis MPE") source that other apps can subscribe to —
# same pattern as xenwooting's "XenWTN" port. If snd-seq isn't loaded
# (rare; missing on some headless / containerised systems), we warn and
# offer the modprobe fix so the user knows what's wrong before xentool
# fails at runtime.
check_alsa_seq() {
    step "Verifying ALSA sequencer (snd-seq) is available"
    if ! command -v aconnect >/dev/null 2>&1; then
        warn "  aconnect not found — install 'alsa-utils' (should already be installed via apt step above)."
        return
    fi
    if aconnect -l >/dev/null 2>&1; then
        echo "  [ok] ALSA seq is available (aconnect -l succeeded)."
    else
        warn "  ALSA sequencer not reachable. Try:  sudo modprobe snd-seq"
        warn "  (xentool needs snd-seq to publish its 'Xentool Wooting' virtual MIDI port.)"
    fi
}

print_post_install_help() {
    hr
    echo "Done."
    echo
    echo "Live HUD:        http://localhost:9099/  (also reachable from your LAN)"
    echo
    echo "Attach to the xentool TUI (foreground) — detach with Ctrl-b d:"
    echo "  ~/.local/bin/xentool-tui"
    echo "  # or:  tmux -L xentool attach -t xentool"
    echo
    echo "Tail logs:"
    echo "  journalctl --user -u xentool -f"
    [[ "${XENHARM_INSTALLED:-0}" == 1 ]] && echo "  journalctl --user -u xenharm -f"
    [[ "${SC_INSTALLED:-0}"      == 1 ]] && echo "  journalctl --user -u xentool-supercollider -f"
    [[ "${STUDIO_INSTALLED:-0}"  == 1 ]] && {
        local sport
        case "$BACKEND" in wooting) sport=9101 ;; *) sport=9100 ;; esac
        echo "  journalctl --user -u xentool-studio -f"
        echo
        echo "Studio web UI: http://localhost:$sport/"
    }
    echo
    echo "Manage services:"
    echo "  systemctl --user status xentool"
    echo "  systemctl --user restart xentool"
    echo "  systemctl --user disable --now xentool   # to stop autostart"
    echo
    if [[ ! ":$PATH:" == *":$HOME/.local/bin:"* ]]; then
        warn "  ~/.local/bin is not on your PATH — open a fresh shell or run:"
        echo  "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    fi
    hr
}

install_linux_main() {
    require_linux
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]}")" && pwd)"
    REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

    local mgr
    mgr="$(detect_pkg_mgr)"
    if [[ "$mgr" != apt ]]; then
        warn "Detected non-apt package manager ($mgr). Auto-install only knows apt; you may need to install build prerequisites manually."
    fi

    print_plan
    if ! yesno "Proceed?" Y; then
        echo "Aborted."
        exit 0
    fi

    if [[ "$mgr" == apt ]]; then
        step "Installing apt prerequisites"
        # build tools + ALSA dev headers + USB + curl (needed by rustup) +
        # git (for build) + tmux (so the xentool service runs in a
        # detachable pty session) + alsa-utils (provides `aconnect`, used
        # below to verify the ALSA sequencer is available at runtime).
        apt_install_missing \
            build-essential pkg-config curl git tmux \
            libasound2-dev libudev-dev libusb-1.0-0-dev \
            libssl-dev alsa-utils
    fi

    check_alsa_seq
    ensure_rust
    build_xentool

    if [[ "$BACKEND" == wooting ]]; then
        install_wooting_sdks
    fi

    setup_xenharm
    setup_supercollider
    setup_studio

    prompt_layout "$LAYOUT_KIND" "$REPO_ROOT/$LAYOUT_KIND"
    # Note: there's no MIDI-output prompt anymore — xentool creates its own
    # virtual ALSA seq port at runtime ("Xentool Wooting" / "Xentool Exquis
    # MPE"), so other apps subscribe to it directly instead of routing
    # through a shared "Midi Through" port. Override at runtime via the
    # systemd unit's ExecStart `--output <name>` if needed.

    step "Writing systemd user units"
    write_xenharm_unit
    write_xentool_unit
    write_supercollider_unit
    write_studio_unit
    write_xentool_tui_helper

    enable_lingering
    start_services
    print_post_install_help
}
