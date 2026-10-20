#!/bin/sh
# Installs TaskMonitor from this tarball into /usr (run with sudo).
# Uninstall: sudo ./install.sh --uninstall
set -eu
cd "$(dirname "$0")"
if [ "${1:-}" = "--uninstall" ]; then
    find usr -type f | while read -r f; do rm -f "/$f"; done
    echo "TaskMonitor removed."
else
    cp -r usr/. /usr/
    echo "TaskMonitor installed. Open it from your app menu or run: taskmonitor"
fi
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
