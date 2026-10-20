#!/bin/sh
# Lays out the installed file tree: stage.sh <binary dir> <dest usr dir> [--no-helper]
set -eu
bin=$1 usr=$2 helper=${3:-}
here=$(dirname "$0")
install -Dm755 "$bin/taskmonitor" "$usr/bin/taskmonitor"
if [ "$helper" != "--no-helper" ]; then
    install -Dm755 "$bin/taskmonitor-helper" "$usr/libexec/taskmonitor-helper"
    install -Dm644 "$here/io.github.taskmonitor.policy" "$usr/share/polkit-1/actions/io.github.taskmonitor.policy"
fi
install -Dm644 "$here/io.github.taskmonitor.desktop" "$usr/share/applications/io.github.taskmonitor.desktop"
install -Dm644 "$here/io.github.taskmonitor.metainfo.xml" "$usr/share/metainfo/io.github.taskmonitor.metainfo.xml"
for dir in "$here"/icons/hicolor/*/apps; do
    size=$(basename "$(dirname "$dir")")
    for f in "$dir"/*; do
        install -Dm644 "$f" "$usr/share/icons/hicolor/$size/apps/$(basename "$f")"
    done
done
install -Dm644 "$here/../crates/app/fonts/OFL.txt" "$usr/share/licenses/taskmonitor/Mona-Sans-OFL.txt"
install -Dm644 "$here/../crates/app/fonts/LUCIDE-LICENSE" "$usr/share/licenses/taskmonitor/Lucide-LICENSE"
