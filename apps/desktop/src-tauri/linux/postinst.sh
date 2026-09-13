#!/bin/sh
# Round 27: bundle.linux.deb.postInstallScript (tauri.conf.json). Runs as
# root during `dpkg -i`/`apt install`, after the package's own files
# (including linux/main.desktop, installed to /usr/share/applications/)
# are already unpacked.
#
# Installing a correctly-tagged .desktop file alone isn't enough - most
# desktop environments and browsers resolve a custom URL scheme against a
# *cached* index (typically /usr/share/applications/mimeinfo.cache), not
# by re-scanning every .desktop file on each launch. Without refreshing
# that cache, a fresh install's MimeType=x-scheme-handler/localsync; entry
# is real but invisible until something else happens to trigger a rebuild
# (a reboot, another package's install, ...) - update-desktop-database is
# the standard, documented way to force that refresh immediately instead
# of leaving it to chance.
#
# `|| true`: update-desktop-database ships in desktop-file-utils, which is
# present on essentially every mainstream desktop Linux system but isn't a
# hard dependency of this package (a minimal/server/WSL install without a
# desktop environment has no use for MIME/URL-scheme handling at all, and
# shouldn't have this package's install fail just because that command is
# missing there) - same "non-fatal on an environment where the feature
# this step supports doesn't even apply" pattern this project already
# uses for round 8's `ufw` check.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi

exit 0
