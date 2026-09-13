#!/bin/sh
# Round 27: bundle.linux.deb.postRemoveScript (tauri.conf.json). Mirrors
# postinst.sh - refreshes the same MIME/desktop-file cache after removal,
# so an uninstalled LocalSync doesn't leave a stale, no-longer-real
# x-scheme-handler/localsync registration pointing at a binary that's
# gone. Same non-fatal reasoning as postinst.sh: harmless no-op on a
# system that never had desktop-file-utils to begin with.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi

exit 0
