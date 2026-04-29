#!/usr/bin/env bash
set -euo pipefail

step() { printf '\033[36m[install]\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m[ok]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
die()  { printf 'Error: %s\n' "$*" >&2; exit 1; }

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_PATH="$UNIT_DIR/xenharm.service"

is_service_dir() {
    local dir="$1"
    [[ -f "$dir/server.py" ]]
}

resolve_service_dir() {
    local candidate

    if [[ $# -gt 0 ]]; then
        candidate="$1"
        if is_service_dir "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
        return 1
    fi

    is_service_dir "$SCRIPT_DIR" || return 1
    printf '%s\n' "$SCRIPT_DIR"
}

if ! command -v systemctl >/dev/null 2>&1; then
    echo "systemctl not found" >&2
    exit 1
fi

if ! command -v python3.12 >/dev/null 2>&1; then
    die "python3.12 not found"
fi

REQUESTED_ROOT="${1:-}"
if [[ -n "$REQUESTED_ROOT" ]]; then
    REQUESTED_ROOT="$(cd -- "$REQUESTED_ROOT" 2>/dev/null && pwd)" || die "service path not found: $1"
fi

SERVICE_DIR="$(resolve_service_dir "$REQUESTED_ROOT")" || {
    if [[ -n "$REQUESTED_ROOT" ]]; then
        die "path must point to the xenharm_service directory: $REQUESTED_ROOT"
    fi
    die "could not find server.py next to install-systemd.sh. Re-run with an explicit path, for example: bash install-systemd.sh /path/to/xenharm_service"
}

step "Installing systemd user unit to $UNIT_PATH"
mkdir -p "$UNIT_DIR"

cat >"$UNIT_PATH" <<EOF
[Unit]
Description=XenHarm note name service (used by xentool's Live HUD)
After=network.target

[Service]
Type=simple
ExecStart=$(command -v python3.12) "$SERVICE_DIR/server.py" --host 127.0.0.1 --port 3199
WorkingDirectory=$SERVICE_DIR
Restart=on-failure
RestartSec=1

[Install]
WantedBy=default.target
EOF

step "Reloading systemd user units"
systemctl --user daemon-reload

step "Enabling and starting xenharm.service"
systemctl --user enable --now xenharm.service

ok "Installed and started xenharm.service"
echo "Using service directory: $SERVICE_DIR"
echo
echo "Status:"
systemctl --user --no-pager --full status xenharm.service || true
echo
echo "Health check:"
echo "  curl -s http://127.0.0.1:3199/health"
echo
warn "If the service fails immediately, make sure xenharmlib is installed for $(command -v python3.12)"
