#!/bin/sh
set -u
uuid="shiftdrop@jayhemnani"
app_id="dev.jayhemnani.ShiftDrop"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
gnome-extensions disable "$uuid" 2>/dev/null
rm -f "$HOME/.local/bin/shiftdrop" "$data/gnome-shell/extensions/$uuid" \
      "$data/dbus-1/services/$app_id.service" "$data/applications/$app_id.desktop" \
      "$data/nautilus/scripts/ShiftDrop" "$HOME/.config/autostart/$app_id.desktop"
echo "removed."
