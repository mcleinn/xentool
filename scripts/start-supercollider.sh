#!/usr/bin/env sh
# Launch SuperCollider with the bundled tanpura patch. Adapted from the
# original launcher in C:\Dev-Free\SuperCollider\mpe_tanpura_xentool_start.sh.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec sclang "$SCRIPT_DIR/../supercollider/mpe_tanpura_xentool.scd"
