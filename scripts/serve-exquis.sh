#!/usr/bin/env bash
# Run `xentool serve` against an Exquis layout.
#
# Usage:
#   bash scripts/serve-exquis.sh                   # default: xtn/edo53.xtn
#   bash scripts/serve-exquis.sh xtn/edo24.xtn
#   bash scripts/serve-exquis.sh xtn/edo31.xtn --mts-esp
#   bash scripts/serve-exquis.sh xtn/edo31.xtn --pb-range 48
#
# All arguments are forwarded to `xentool serve` verbatim. The script `cd`s
# into the project root so relative paths like `xtn/foo.xtn` resolve.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

if [[ $# -eq 0 ]]; then
    set -- xtn/edo53.xtn
fi

exec xentool serve "$@"
