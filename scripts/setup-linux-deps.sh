#!/usr/bin/env bash
# One-time setup for building/running LocalSync on Linux (or WSL2 Ubuntu).
# Needs sudo interactively, so run this yourself rather than via an agent:
#
#   bash scripts/setup-linux-deps.sh
set -euo pipefail

sudo apt update
sudo apt install -y \
    build-essential curl wget file pkg-config \
    libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev \
    podman podman-compose uidmap slirp4netns

echo
echo "Done. Verify with:"
echo "  podman --version && podman-compose --version"
echo "  podman run --rm hello-world   # confirms rootless podman actually works"
