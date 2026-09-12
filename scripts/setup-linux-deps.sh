#!/usr/bin/env bash
# One-time setup for building/running LocalSync on Linux (or WSL2 Ubuntu).
# Needs sudo interactively, so run this yourself rather than via an agent:
#
#   bash scripts/setup-linux-deps.sh
set -euo pipefail

# podman-compose isn't in every distro/release's apt repos (it landed in
# Ubuntu's universe repo relatively recently, and some minimal setups don't
# have universe enabled at all) — and `apt install` with even one
# unresolvable package name refuses to install *any* of the list, real
# friction reported from actual use: everything else in this line
# (build-essential, the GTK/webkit dev headers, podman itself) silently
# never got installed either, because apt bailed on podman-compose before
# installing anything. Split so a missing/unavailable podman-compose can't
# take the rest of this list down with it, and so its own fallback (below)
# runs regardless of how the main apt install went.
sudo apt update
sudo apt install -y \
    build-essential curl wget file pkg-config \
    libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev \
    podman uidmap slirp4netns

# Try apt first (it's the real package on modern Ubuntu/Debian - confirmed
# present in 24.04's universe repo); `|| true` so an unresolvable package
# name here doesn't stop this script, since the fallback below covers it.
sudo apt install -y podman-compose || true

if ! command -v podman-compose >/dev/null 2>&1; then
    echo "podman-compose not available via apt on this system — falling back to pip/pipx" >&2
    # Same class of fallback already proven for Windows (see
    # crates/ls-containers/src/provisioning/windows_impl.rs) - the podman
    # installer/apt package for this tool isn't universal, a Python-based
    # install is. `pipx`, not a bare `pip3 install --user`: verified by
    # actually running both on a real Ubuntu 24.04 box while writing this -
    # plain `pip3 install --user podman-compose` fails outright here with
    # "externally-managed-environment" (PEP 668, enforced by default on
    # Debian/Ubuntu since ~2023) whether or not `--user` is passed. `pipx`
    # is the tool PEP 668's own error message points at for exactly this
    # case (installing a standalone Python CLI application) and its
    # `ensurepath` puts the install directory on PATH for future shells
    # automatically, rather than leaving that as a step for the human.
    if ! command -v pipx >/dev/null 2>&1; then
        sudo apt install -y pipx
    fi
    pipx install podman-compose
    pipx ensurepath
    if ! command -v podman-compose >/dev/null 2>&1; then
        echo
        echo "podman-compose installed via pipx, but isn't on this shell's PATH yet."
        echo "Open a new terminal (pipx ensurepath already updated your shell profile for the next one)."
    fi
fi

# Round 22: the Send wizard's database export step needs each engine's own
# real client tool - not needed to build/run the app itself, but a missing
# one now gets a clear, actionable error from the app itself (see
# crates/ls-dbsource/src/engines/postgres.rs's/mongo.rs's
# missing_pg_dump_hint/missing_mongodump_hint) that points back here. MySQL/
# MariaDB needs none of this: its connect/list/export are pure-Rust (no
# libmysqlclient/system dependency at all - see that engine's own Cargo.toml
# comment), so there's nothing to install for it.
echo
echo "Installing PostgreSQL client tools (pg_dump/psql, needed for the Send wizard's PostgreSQL export)..."
sudo apt install -y postgresql-client

if ! command -v mongodump >/dev/null 2>&1; then
    echo
    echo "MongoDB Database Tools (mongodump/mongorestore) aren't in Debian/Ubuntu/Kali's"
    echo "standard apt repos, so this needs a direct download from MongoDB - confirmed"
    echo "reachable and working (real .deb, extracted with dpkg-deb, no root needed for"
    echo "that part) while building this round:"
    echo
    echo "  curl -sL -o /tmp/mongodb-tools.deb \\"
    echo "    https://fastdl.mongodb.org/tools/db/mongodb-database-tools-debian12-x86_64-100.10.0.deb"
    echo "  sudo apt install -y /tmp/mongodb-tools.deb"
    echo
    echo "(swap debian12 for ubuntu2204/ubuntu2404/etc. if you're not on Debian/Kali -"
    echo "see https://www.mongodb.com/try/download/database-tools for the exact package"
    echo "for your distro.) Not run automatically here since it fetches a specific"
    echo "package build for a specific distro - safer to confirm you're getting the"
    echo "right one than to guess."
fi

echo
echo "Done. Verify with:"
echo "  podman --version && podman-compose --version"
echo "  podman run --rm hello-world   # confirms rootless podman actually works"
echo "  pg_dump --version             # PostgreSQL export support"
echo "  mongodump --version           # MongoDB export support (if installed above)"
