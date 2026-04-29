#!/usr/bin/env bash
# Linux installer for the Exquis backend of xentool.
#
# Targets Patchbox OS / Raspberry Pi OS / Ubuntu on Pi 4/5.
# Builds and installs xentool, optionally sets up the xenharm sidecar
# (for microtonal note glyphs) and the SuperCollider tanpura synth, then
# wires everything as systemd user services.
#
# Usage:
#   bash scripts/install-linux-exquis.sh

set -euo pipefail

BACKEND="exquis"
LAYOUT_KIND="xtn"

# shellcheck disable=SC1091
source "$(dirname "${BASH_SOURCE[0]}")/install-linux-common.sh"

install_linux_main
