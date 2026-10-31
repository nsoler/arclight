#!/usr/bin/env bash
# Remove what install.sh added: the binaries, the example clip, the desktop
# entry and the icons. Settings in ~/.config/arclight are left alone.
set -euo pipefail

prefix="${PREFIX:-$HOME/.local}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
icons="$data/icons/hicolor"

rm -f "$prefix/bin/arclight-gui" "$prefix/bin/arclight" "$data/applications/arclight.desktop" \
  "$icons/scalable/apps/arclight.svg" \
  "$icons"/{16x16,24x24,32x32,48x48,64x64,128x128,256x256}/apps/arclight.png
rm -rf "$prefix/share/arclight"

command -v update-desktop-database >/dev/null && update-desktop-database -q "$data/applications" || true
command -v gtk-update-icon-cache >/dev/null && [[ -d "$icons" ]] && gtk-update-icon-cache -q -f -t "$icons" || true
echo "Removed ArclightVFX."
