#!/bin/sh
# Builds LscSync-<version>-amd64.deb in target/release/bundle/ (Linux): the
# package Ubuntu, Debian and Mint install with a double-click. Its contents
# and metadata are under [package.metadata.deb] in crates/app/Cargo.toml.
# usage: ./tools/deb.sh [--skip-build]
#   --skip-build  reuse target/release/lscsync (CI)
#
# Built on Ubuntu 24.04, it needs a distribution of that age or newer
# (Ubuntu 24.04, Debian 13, Mint 22).
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
BUNDLE=target/release/bundle
OUTPUT="$BUNDLE/LscSync-$VERSION-amd64.deb"

if [ "${1:-}" != "--skip-build" ]; then
    cargo build --release --locked -p lscsync-app
fi
if ! cargo deb --version >/dev/null 2>&1; then
    cargo install cargo-deb --locked
fi

mkdir -p "$BUNDLE"
rm -f "$OUTPUT"
cargo deb -p lscsync-app --no-build --output "$OUTPUT"
echo "deb: $OUTPUT"
