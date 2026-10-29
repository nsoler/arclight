#!/usr/bin/env bash
# Add Arclight to the app launcher (Omarchy's, or any freedesktop desktop's)
# for this user: a desktop entry and icons under ~/.local/share. The entry
# runs the release build in this checkout, so later `cargo build --release`
# runs are picked up without reinstalling. Undo with ./uninstall.sh.
#
#   ./install.sh            build if needed, then install
#   ./install.sh --build    rebuild the release binary first, then install
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="$repo/target/release/arclight-gui"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
apps="$data/applications"
icons="$data/icons/hicolor"

if [[ "${1:-}" == "--build" || ! -x "$bin" ]]; then
  echo "Building Arclight (release)…"
  cargo build --release --manifest-path "$repo/Cargo.toml" --bin arclight-gui
fi

# Icons. Launchers draw app icons small (Omarchy's at ~20-32 px, from the
# scalable SVG), where the ARCLIGHT lettering can't be read, so the
# scalable icon and the small PNGs are the logo without it
# (arclight-icon.svg); 128 px and up show the full logo (arclight.svg).
install -Dm644 "$repo/assets/arclight-icon.svg" "$icons/scalable/apps/arclight.svg"
for size in 16 24 32 48 64 128 256; do
  src="$repo/assets/arclight-icon.svg"
  (( size >= 128 )) && src="$repo/assets/arclight.svg"
  dest="$icons/${size}x${size}/apps/arclight.png"
  mkdir -p "$(dirname "$dest")"
  if command -v rsvg-convert >/dev/null; then
    rsvg-convert -w "$size" -h "$size" "$src" -o "$dest"
  elif command -v magick >/dev/null; then
    magick -background none "$src" -resize "${size}x${size}" "$dest"
  elif [[ "$size" == 256 ]]; then
    install -m644 "$repo/assets/arclight.png" "$dest"
  fi
done

# The desktop entry. Its name matches the window's app id ("arclight").
mkdir -p "$apps"
cat > "$apps/arclight.desktop" <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=Arclight
GenericName=Node-based Compositor
Comment=Node-graph image and video compositor (in development)
Exec="$bin" %f
TryExec=$bin
Path=$repo
Icon=arclight
Terminal=false
StartupNotify=true
StartupWMClass=arclight
Categories=Graphics;2DGraphics;RasterGraphics;
Keywords=compositing;vfx;nodes;keyer;roto;tracker;
DESKTOP

# Refresh the caches where the tools exist (Omarchy's launcher re-scans
# desktop entries on its own).
command -v update-desktop-database >/dev/null && update-desktop-database -q "$apps" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -f -t "$icons" || true

echo "Installed: $apps/arclight.desktop"
echo "Arclight is now in the app launcher. Remove it with ./uninstall.sh"
