#!/usr/bin/env bash
# Build Arclight for Linux (x86_64) and package it as a tarball:
#   dist/arclight-<ver>-linux-x86_64.tar.gz
# containing
#   bin/arclight-gui, bin/arclight            the GUI and the CLI / MCP server
#   share/arclight/examples/media/testsrc.mp4 the starter project's clip
#   share/arclight/icons/                     the icons
#   install.sh, uninstall.sh                  per-user install under ~/.local
#
# Run from anywhere: packaging/linux/build-tarball.sh
# The binaries link only glibc (graphics and windowing libraries are loaded
# at run time), and the GUI libasound.so.2 (ALSA's library, for sound; any
# desktop with PipeWire or PulseAudio has it), so they run on the
# distribution they were built on and newer ones. ffmpeg isn't bundled:
# install the distribution's package.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
arch="$(uname -m)"
name="arclight-$version-linux-$arch"
dist="$repo/dist"
stage="$dist/$name"

echo "Building Arclight $version for Linux $arch..."
cargo build --release --manifest-path "$repo/Cargo.toml" --bin arclight-gui --bin arclight

rm -rf "$stage" && mkdir -p "$stage/bin" "$stage/share/arclight/examples/media" "$stage/share/arclight/icons"
install -m755 "$repo/target/release/arclight-gui" "$repo/target/release/arclight" "$stage/bin/"
strip "$stage/bin/"* 2>/dev/null || true
install -m644 "$repo/examples/media/testsrc.mp4" "$stage/share/arclight/examples/media/"
install -m644 "$repo/assets/arclight.svg" "$repo/assets/arclight-icon.svg" "$repo/assets/arclight.png" "$stage/share/arclight/icons/"
install -m755 "$repo/packaging/linux/install.sh" "$repo/packaging/linux/uninstall.sh" "$stage/"
install -m644 "$repo/CHANGELOG.md" "$stage/"

tarball="$dist/$name.tar.gz"
tar -C "$dist" -czf "$tarball" "$name"
rm -rf "$stage"

echo "Done: $tarball"
