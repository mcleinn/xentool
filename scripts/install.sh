#!/usr/bin/env bash
# Build and install xentool to ~/.cargo/bin/.
#
# Usage: bash scripts/install.sh
#
# This is a thin wrapper around `cargo install --path . --force`.
# Wooting SDKs (only required for the Wooting backend) are installed
# separately via scripts/install-wooting-sdks.sh.

set -euo pipefail

step() { printf '\033[36m[install]\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m[ok]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

step "Building and installing xentool from $PROJECT_ROOT"
cargo install --path . --force

ok "Installed to ~/.cargo/bin/xentool"
echo
echo "Open a fresh shell, then verify with:  xentool --version"
echo
warn "For Wooting backend support, also run:  bash $SCRIPT_DIR/install-wooting-sdks.sh"
