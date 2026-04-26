#!/usr/bin/env bash
# Run `xentool serve` against a Wooting layout.
#
# Usage:
#   bash scripts/serve-wooting.sh                  # default: wtn/edo31.wtn
#   bash scripts/serve-wooting.sh wtn/edo24.wtn
#   bash scripts/serve-wooting.sh wtn/edo31.wtn --output "loopMIDI Port"
#
# All arguments are forwarded to `xentool serve` verbatim. The script `cd`s
# into the project root so relative paths like `wtn/foo.wtn` resolve.
#
# Requires the Wooting Analog and RGB SDKs to be installed
# (scripts/install-wooting-sdks.sh).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

if [[ $# -eq 0 ]]; then
    set -- wtn/edo31.wtn
fi

exec xentool serve "$@"
