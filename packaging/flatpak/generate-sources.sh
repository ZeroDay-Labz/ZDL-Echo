#!/usr/bin/env bash
# Regenerate packaging/flatpak/generated-sources.json from the current
# Cargo.lock. Flatpak builds are network-isolated, so every crate source
# has to be pre-declared with a URL + hash; this script (from
# flatpak/flatpak-builder-tools) reads Cargo.lock and emits that manifest.
#
# Run this once, and again any time Cargo.lock changes, then commit the
# result alongside the lockfile.
set -euo pipefail
cd "$(dirname "$0")/../.."

TOOL_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py"
TMP_SCRIPT="$(mktemp --suffix=.py)"
trap 'rm -f "$TMP_SCRIPT"' EXIT

curl -sL -o "$TMP_SCRIPT" "$TOOL_URL"
pip3 install --user --quiet tomlkit pyyaml aiohttp
python3 "$TMP_SCRIPT" Cargo.lock -o packaging/flatpak/generated-sources.json

echo "wrote packaging/flatpak/generated-sources.json"
