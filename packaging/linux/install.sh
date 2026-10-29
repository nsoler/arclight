#!/usr/bin/env bash
# Install Arclight from this release for the current user: the binaries in
# ~/.local/bin, the example clip in ~/.local/share/arclight, and a desktop
# entry and icons so it shows in the app launcher (Omarchy's, or any
# freedesktop desktop's). No root needed. Undo with ./uninstall.sh.
# Set PREFIX to install somewhere other than ~/.local.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
apps="$data/applications"
icons="$data/icons/hicolor"

install -Dm755 "$here/bin/arclight-gui" "$prefix/bin/arclight-gui"
install -Dm755 "$here/bin/arclight" "$prefix/bin/arclight"
# The GUI finds the starter clip at ../share/arclight beside its binary.
install -Dm644 "$here/share/arclight/examples/media/testsrc.mp4" \
  "$prefix/share/arclight/examples/media/testsrc.mp4"

# Icons: the logo without lettering where launchers draw it small, the
# full logo from 128 px up.
install -Dm644 "$here/share/arclight/icons/arclight-icon.svg" "$icons/scalable/apps/arclight.svg"
for size in 16 24 32 48 64 128 256; do
  src="$here/share/arclight/icons/arclight-icon.svg"
  (( size >= 128 )) && src="$here/share/arclight/icons/arclight.svg"
  dest="$icons/${size}x${size}/apps/arclight.png"
  mkdir -p "$(dirname "$dest")"
  if command -v rsvg-convert >/dev/null; then
    rsvg-convert -w "$size" -h "$size" "$src" -o "$dest"
  elif command -v magick >/dev/null; then
    magick -background none "$src" -resize "${size}x${size}" "$dest"
  elif [[ "$size" == 256 ]]; then
    install -m644 "$here/share/arclight/icons/arclight.png" "$dest"
  fi
done

mkdir -p "$apps"
cat > "$apps/arclight.desktop" <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=Arclight
GenericName=Node-based Compositor
Comment=Node-graph image and video compositor (in development)
Exec="$prefix/bin/arclight-gui" %f
TryExec=$prefix/bin/arclight-gui
Icon=arclight
Terminal=false
StartupNotify=true
StartupWMClass=arclight
Categories=Graphics;2DGraphics;RasterGraphics;
Keywords=compositing;vfx;nodes;keyer;roto;tracker;
DESKTOP

command -v update-desktop-database >/dev/null && update-desktop-database -q "$apps" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -f -t "$icons" || true

echo "Installed Arclight in $prefix/bin and the app launcher."
command -v ffmpeg >/dev/null || echo "Note: video needs ffmpeg (e.g. sudo pacman -S ffmpeg)."
