#!/usr/bin/env bash
# Install Wooting Analog SDK + Wooting RGB SDK on Linux (x86_64 or aarch64).
#
# Usage: bash scripts/install-wooting-sdks.sh

set -euo pipefail

step() { printf '\033[36m[install]\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m[ok]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }

ARCH="$(uname -m)"
WORK="${TMPDIR:-/tmp}/xentool-wooting-install"
mkdir -p "$WORK"
step "Work dir: $WORK"

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 1; }; }
need curl
need unzip
need tar

gh_latest_asset_url() {
    local repo="$1" regex="$2"
    curl -sSL "https://api.github.com/repos/$repo/releases/latest" \
      | grep -oE '"browser_download_url":\s*"[^"]+"' \
      | sed -E 's/.*"(https[^"]+)".*/\1/' \
      | grep -E "$regex" | head -n1
}

# --- 1. Analog SDK ---
if [[ -f /usr/lib/x86_64-linux-gnu/libwooting_analog_sdk.so ]] || \
   [[ -f /usr/local/lib/libwooting_analog_sdk.so ]]; then
    ok "Wooting Analog SDK already installed."
else
    if command -v dpkg >/dev/null 2>&1 && [[ "$ARCH" == "x86_64" ]]; then
        url=$(gh_latest_asset_url WootingKb/wooting-analog-sdk '\.deb$')
        deb="$WORK/$(basename "$url")"
        step "Downloading $url"
        curl -sSL -o "$deb" "$url"
        step "Installing $deb (sudo)"
        sudo dpkg -i "$deb" || sudo apt-get install -f -y
        ok "Analog SDK installed."
    else
        case "$ARCH" in
            x86_64)  regex='x86_64-unknown-linux-gnu\.tar\.gz$' ;;
            aarch64) regex='aarch64-.*linux.*\.tar\.gz$' ;;
            *) echo "Unsupported arch: $ARCH" >&2; exit 1 ;;
        esac
        url=$(gh_latest_asset_url WootingKb/wooting-analog-sdk "$regex")
        [[ -n "$url" ]] || { echo "No Analog SDK asset for $ARCH in latest release" >&2; exit 1; }
        tgz="$WORK/$(basename "$url")"
        step "Downloading $url"
        curl -sSL -o "$tgz" "$url"
        step "Extracting"
        mkdir -p "$WORK/analog"
        tar -xzf "$tgz" -C "$WORK/analog"
        so=$(find "$WORK/analog" -name 'libwooting_analog_sdk*.so*' -print -quit)
        [[ -n "$so" ]] || { echo "libwooting_analog_sdk.so not found in archive" >&2; exit 1; }
        sudo install -D "$so" /usr/local/lib/libwooting_analog_sdk.so
        sudo ldconfig
        ok "Analog SDK installed to /usr/local/lib."
    fi
fi

# --- 2. RGB SDK ---
if [[ -f /usr/local/lib/libwooting-rgb-sdk.so ]] || \
   [[ -f /usr/lib/libwooting-rgb-sdk.so ]]; then
    ok "Wooting RGB SDK already installed."
else
    url=$(gh_latest_asset_url WootingKb/wooting-rgb-sdk 'ubuntu-x64\.zip$')
    [[ -n "$url" ]] || { echo "No RGB SDK zip found" >&2; exit 1; }
    zip="$WORK/$(basename "$url")"
    step "Downloading $url"
    curl -sSL -o "$zip" "$url"
    step "Extracting"
    rm -rf "$WORK/rgb"; mkdir "$WORK/rgb"
    unzip -q "$zip" -d "$WORK/rgb"
    so=$(find "$WORK/rgb" -name 'libwooting-rgb-sdk*.so*' -print -quit)
    [[ -n "$so" ]] || { echo "libwooting-rgb-sdk.so not found in archive" >&2; exit 1; }
    sudo install -D "$so" /usr/local/lib/libwooting-rgb-sdk.so
    sudo ldconfig
    ok "RGB SDK installed."
fi

ok "Done. Run: xentool list"
