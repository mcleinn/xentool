#!/usr/bin/env bash
# Linux installer for the Wooting backend of xentool.
#
# Targets Patchbox OS / Raspberry Pi OS / Ubuntu on Pi 4/5.
# In addition to building xentool and (optionally) setting up xenharm and
# SuperCollider, this also installs the Wooting Analog and RGB SDKs into
# /usr/local/lib so xentool can load them at runtime.
#
# Usage:
#   bash scripts/install-linux-wooting.sh

set -euo pipefail

BACKEND="wooting"
LAYOUT_KIND="wtn"

# shellcheck disable=SC1091
source "$(dirname "${BASH_SOURCE[0]}")/install-linux-common.sh"

install_linux_main
