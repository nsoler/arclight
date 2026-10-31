#!/usr/bin/env bash
# Remove ArclightVFX from the app launcher: the desktop entry and icons that
# install.sh added under ~/.local/share. The checkout and its build are left
# alone.
set -euo pipefail

data="${XDG_DATA_HOME:-$HOME/.local/share}"
apps="$data/applications"
icons="$data/icons/hicolor"

removed=0
for f in "$apps/arclight.desktop" "$icons/scalable/apps/arclight.svg" \
         "$icons"/{16x16,24x24,32x32,48x48,64x64,128x128,256x256}/apps/arclight.png; do
  if [[ -e "$f" ]]; then
    rm -f "$f"
    removed=$((removed + 1))
  fi
done

command -v update-desktop-database >/dev/null && update-desktop-database -q "$apps" || true
command -v gtk-update-icon-cache >/dev/null && [[ -d "$icons" ]] && gtk-update-icon-cache -q -f -t "$icons" || true

if (( removed )); then
  echo "Removed ArclightVFX from the app launcher ($removed files)."
else
  echo "ArclightVFX wasn't installed in the app launcher; nothing to remove."
fi
