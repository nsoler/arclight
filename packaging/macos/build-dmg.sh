#!/usr/bin/env bash
# Build Arclight for macOS and package it as a DMG:
#   dist/Arclight.app                 the app (GUI, plus the `arclight` CLI inside)
#   dist/Arclight-<ver>-universal.dmg drag-to-Applications installer
# The binaries are universal: built for Apple Silicon (arm64) and Intel
# (x86_64) and joined with lipo, so one app runs natively on any Mac from
# macOS 11. ARCHS="arm64" (or "x86_64") builds one architecture only, and
# names the DMG after it.
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
archs="${ARCHS:-arm64 x86_64}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
dist="$repo/dist"
app="$dist/Arclight.app"

[[ "$(uname -s)" == Darwin ]] || { echo "build-dmg.sh runs on macOS (it needs Apple's SDK, iconutil and hdiutil)." >&2; exit 1; }
command -v rsvg-convert >/dev/null || { echo "rsvg-convert not found: brew install librsvg" >&2; exit 1; }

# Big Sur (11) is the first macOS on Apple Silicon; Intel builds use the
# same floor so both halves of the universal app agree.
export MACOSX_DEPLOYMENT_TARGET=11.0
for arch in $archs; do
  if [[ "$arch" == arm64 ]]; then target=aarch64-apple-darwin; else target=x86_64-apple-darwin; fi
  echo "Building Arclight ${version} for ${target}..."
  rustup target add "$target" >/dev/null 2>&1 || true
  cargo build --release --manifest-path "$repo/Cargo.toml" --target "$target" --bin arclight-gui --bin arclight
done

echo "Assembling Arclight.app..."
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/examples/media"
# One binary per program, holding every architecture built.
for bin in arclight-gui arclight; do
  slices=()
  for arch in $archs; do
    if [[ "$arch" == arm64 ]]; then target=aarch64-apple-darwin; else target=x86_64-apple-darwin; fi
    slices+=("$repo/target/$target/release/$bin")
  done
  lipo -create "${slices[@]}" -output "$app/Contents/MacOS/$bin"
  chmod 755 "$app/Contents/MacOS/$bin"
done
install -m644 "$repo/examples/media/testsrc.mp4" "$app/Contents/Resources/examples/media/"

# The icon: the logo without lettering at small sizes, where the word can't
# be read, the full logo from 128 px up.
iconset="$dist/arclight.iconset"
rm -rf "$iconset" && mkdir -p "$iconset"
# (Plain ifs: under set -e, a test that's false inside $(...) or as the
# last command would end the script.)
for size in 16 32 128 256 512; do
  for scale in 1 2; do
    px=$((size * scale))
    if (( px >= 128 )); then src="$repo/assets/arclight.svg"; else src="$repo/assets/arclight-icon.svg"; fi
    if (( scale == 2 )); then name="icon_${size}x${size}@2x.png"; else name="icon_${size}x${size}.png"; fi
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
if [[ "$archs" == *" "* ]]; then flavor=universal; else flavor="$archs"; fi
dmg="$dist/Arclight-$version-$flavor.dmg"
stage="$dist/dmg"
rm -rf "$stage" "$dmg" && mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -quiet -volname "Arclight $version" -srcfolder "$stage" -ov -format UDZO "$dmg"
rm -rf "$stage"

echo "Done:"
echo "  $app"
echo "  $dmg"
