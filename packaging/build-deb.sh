#!/bin/sh
# Builds the .deb: target/debian/taskmonitor_<version>_<arch>.deb
# Needs: cargo install cargo-deb
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p taskmonitor -p taskmonitor-helper
cargo deb -p taskmonitor --no-build "$@"
