#!/usr/bin/env bash
# Build Arclight for Apple Silicon macOS and package it as a DMG:
#   dist/Arclight.app            the app (GUI, plus the `arclight` CLI inside)
#   dist/Arclight-<ver>-arm64.dmg drag-to-Applications installer
#
# Run on a Mac (Apple's SDK and tools only run there), from anywhere:
#   packaging/macos/build-dmg.sh
# Needs: Rust (rustup), Xcode command line tools, and rsvg-convert
# (`brew install librsvg`) to draw the icon. ffmpeg isn't bundled: users
# install it with `brew install ffmpeg`, and Arclight finds it there.
#
# The app is signed ad hoc (Apple Silicon refuses to run unsigned code), not
# with a Developer ID, so a downloaded copy opens with right-click → Open
# the first time (or after `xattr -dr com.apple.quarantine Arclight.app`).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target=aarch64-apple-darwin
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
dist="$repo/dist"
app="$dist/Arclight.app"

[[ "$(uname -s)" == Darwin ]] || { echo "build-dmg.sh runs on macOS (it needs Apple's SDK, iconutil and hdiutil)." >&2; exit 1; }
command -v rsvg-convert >/dev/null || { echo "rsvg-convert not found: brew install librsvg" >&2; exit 1; }

echo "Building Arclight ${version} for ${target}..."
rustup target add "$target" >/dev/null 2>&1 || true
# Big Sur (11) is the first macOS on Apple Silicon.
export MACOSX_DEPLOYMENT_TARGET=11.0
cargo build --release --manifest-path "$repo/Cargo.toml" --target "$target" --bin arclight-gui --bin arclight

echo "Assembling Arclight.app..."
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/examples/media"
install -m755 "$repo/target/$target/release/arclight-gui" "$app/Contents/MacOS/arclight-gui"
install -m755 "$repo/target/$target/release/arclight" "$app/Contents/MacOS/arclight"
install -m644 "$repo/examples/media/testsrc.mp4" "$app/Contents/Resources/examples/media/"

# The icon: the logo without lettering at small sizes, where the word can't
# be read, the full logo from 128 px up.
iconset="$dist/arclight.iconset"
rm -rf "$iconset" && mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  for scale in 1 2; do
    px=$((size * scale))
    src="$repo/assets/arclight-icon.svg"
    (( px >= 128 )) && src="$repo/assets/arclight.svg"
    name="icon_${size}x${size}$([[ $scale == 2 ]] && echo @2x).png"
    rsvg-convert -w "$px" -h "$px" "$src" -o "$iconset/$name"
  done
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/arclight.icns"
rm -rf "$iconset"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Arclight</string>
  <key>CFBundleDisplayName</key><string>Arclight</string>
  <key>CFBundleIdentifier</key><string>com.github.nsoler.arclight</string>
  <key>CFBundleExecutable</key><string>arclight-gui</string>
  <key>CFBundleIconFile</key><string>arclight</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>MIT OR Apache-2.0</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

echo "Signing (ad hoc)..."
codesign --force --sign - "$app/Contents/MacOS/arclight"
codesign --force --sign - "$app"
codesign --verify --strict "$app"

echo "Making the DMG..."
dmg="$dist/Arclight-$version-arm64.dmg"
stage="$dist/dmg"
rm -rf "$stage" "$dmg" && mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -quiet -volname "Arclight $version" -srcfolder "$stage" -ov -format UDZO "$dmg"
rm -rf "$stage"

echo "Done:"
echo "  $app"
echo "  $dmg"
