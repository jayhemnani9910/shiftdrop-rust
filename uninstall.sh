#!/bin/sh
# Removes what install.sh added. Works the same piped:
#   curl -fsSL https://raw.githubusercontent.com/jayhemnani9910/shiftdrop-rust/main/uninstall.sh | sh
set -u
uuid="shiftdrop@jayhemnani"
app_id="dev.jayhemnani.ShiftDrop"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
gnome-extensions disable "$uuid" 2>/dev/null
# the extension is a link (from a checkout) or a copied folder (from a download)
rm -rf "$data/gnome-shell/extensions/$uuid"
rm -f "$HOME/.local/bin/shiftdrop" \
      "$data/dbus-1/services/$app_id.service" "$data/applications/$app_id.desktop" \
      "$data/nautilus/scripts/ShiftDrop" "$HOME/.config/autostart/$app_id.desktop"
echo "removed."
