#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

if [[ ! -f "$SCRIPT_DIR/server.py" ]]; then
    echo "Could not find server.py next to this script." >&2
    exit 1
fi

exec python3.12 "$SCRIPT_DIR/server.py" "$@"
