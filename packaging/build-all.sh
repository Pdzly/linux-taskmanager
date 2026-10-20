#!/bin/sh
# Builds TaskMonitor for (nearly) every Linux distro into dist/:
#   TaskMonitor-<ver>-x86_64.AppImage   any distro, no install needed
#   taskmonitor_<ver>-1_amd64.deb       Debian, Ubuntu, Mint, Pop!_OS
#   taskmonitor-<ver>-1.x86_64.rpm      Fedora, RHEL, openSUSE
#   taskmonitor-<ver>-x86_64.tar.gz     anything else (install.sh inside)
# Arch users build from packaging/aur/PKGBUILD.
#
# Binaries are linked against glibc 2.28 (2018) so they run on any distro since.
# Needs: zig, squashfs-tools, cargo-zigbuild, cargo-deb, cargo-generate-rpm.
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
TARGET=x86_64-unknown-linux-gnu
GLIBC=2.28
BIN=target/$TARGET/release
DIST=dist
mkdir -p "$DIST"

echo "==> Building $VERSION against glibc $GLIBC"
cargo zigbuild --release --target "$TARGET.$GLIBC" -p taskmonitor -p taskmonitor-helper

# Refuse to ship a binary that needs a newer glibc than promised.
newest=$(objdump -T "$BIN/taskmonitor" "$BIN/taskmonitor-helper" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
echo "    newest glibc symbol: $newest"
[ "$(printf '%s\n' "GLIBC_$GLIBC" "$newest" | sort -V | tail -1)" = "GLIBC_$GLIBC" ] || {
    echo "error: binaries need $newest, more than glibc $GLIBC" >&2
    exit 1
}

echo "==> .deb"
cargo deb -p taskmonitor --no-build --target "$TARGET" -o "$DIST/"

echo "==> .rpm"
cargo generate-rpm -p crates/app --target "$TARGET" -o "$DIST/"

echo "==> tarball"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root="$stage/taskmonitor-$VERSION"
packaging/stage.sh "$BIN" "$root/usr"
cp packaging/install.sh "$root/"
tar -C "$stage" -czf "$DIST/taskmonitor-$VERSION-x86_64.tar.gz" "taskmonitor-$VERSION"

echo "==> AppImage"
appdir="$stage/AppDir"
packaging/stage.sh "$BIN" "$appdir/usr" --no-helper
cp packaging/io.github.taskmonitor.desktop "$appdir/"
cp packaging/icons/hicolor/256x256/apps/io.github.taskmonitor.png "$appdir/io.github.taskmonitor.png"
ln -s io.github.taskmonitor.png "$appdir/.DirIcon"
ln -s usr/bin/taskmonitor "$appdir/AppRun"
runtime=$(packaging/appimage-runtime.sh)
mksquashfs "$appdir" "$stage/app.squashfs" -root-owned -noappend -comp zstd -quiet
out="$DIST/TaskMonitor-$VERSION-x86_64.AppImage"
cat "$runtime" "$stage/app.squashfs" > "$out"
chmod +x "$out"

echo "==> Done:"
ls -lh "$DIST"
