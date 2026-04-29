#!/usr/bin/env sh
# Launch SuperCollider with one of the bundled patches. First argument
# is the patch basename inside `supercollider/`; defaults to
# `mpe_tanpura_xentool.scd` so running the script directly behaves the
# same as before.
#
# Usage:
#   start-supercollider.sh
#   start-supercollider.sh midi_piano_xentool.scd

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SCRIPT_NAME=${1:-mpe_tanpura_xentool.scd}
PATCH="$SCRIPT_DIR/../supercollider/$SCRIPT_NAME"

if [ ! -f "$PATCH" ]; then
    echo "SuperCollider patch not found: $PATCH" >&2
    exit 1
fi

exec sclang "$PATCH"
