#!/bin/sh
# Prints the path of the AppImage runtime (the small launcher at the front of every
# AppImage), downloading the pinned release once and checking its checksum.
set -eu
TAG=20251108
SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
cache="$(dirname "$0")/../target/appimage-runtime-$TAG"
if [ ! -f "$cache" ]; then
    curl -fsSL -o "$cache.part" "https://github.com/AppImage/type2-runtime/releases/download/$TAG/runtime-x86_64"
    echo "$SHA256  $cache.part" | sha256sum -c --quiet - >&2
    mv "$cache.part" "$cache"
fi
echo "$cache"
