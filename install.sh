#!/bin/sh
# Installs for the current user only. Builds the binary, then everything is a
# symlink or a small text file pointing back at this checkout; uninstall.sh
# removes exactly these.
set -eu
repo="$(cd "$(dirname "$0")" && pwd)"
uuid="shiftdrop@jayhemnani"
app_id="dev.jayhemnani.ShiftDrop"
data="${XDG_DATA_HOME:-$HOME/.local/share}"

cargo build --release --manifest-path "$repo/Cargo.toml"
bin="$repo/target/release/shiftdrop"

mkdir -p "$HOME/.local/bin" "$data/gnome-shell/extensions" "$data/dbus-1/services" \
         "$data/applications" "$data/nautilus/scripts"

ln -sfn "$bin" "$HOME/.local/bin/shiftdrop"
ln -sfn "$repo/extension/$uuid" "$data/gnome-shell/extensions/$uuid"

# lets the extension start the app on the first Shift-drag
cat > "$data/dbus-1/services/$app_id.service" <<SERVICE
[D-BUS Service]
Name=$app_id
Exec=$HOME/.local/bin/shiftdrop --gapplication-service
SERVICE

cat > "$data/applications/$app_id.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=ShiftDrop
Comment=Drag a file, hold Shift, pick a format
Exec=$HOME/.local/bin/shiftdrop %F
Terminal=false
NoDisplay=true
DESKTOP

# start at login, so the first drag doesn't wait for GTK to start
mkdir -p "$HOME/.config/autostart"
cat > "$HOME/.config/autostart/$app_id.desktop" <<AUTOSTART
[Desktop Entry]
Type=Application
Name=ShiftDrop
Exec=$HOME/.local/bin/shiftdrop --gapplication-service
NoDisplay=true
X-GNOME-Autostart-enabled=true
AUTOSTART

# Files: right-click > Scripts > ShiftDrop
ln -sfn "$bin" "$data/nautilus/scripts/ShiftDrop"

gnome-extensions enable "$uuid" 2>/dev/null && echo "extension enabled" || \
  echo "log out and back in once, then run: gnome-extensions enable $uuid"
echo "installed. drag a file, hold Shift, drop it on a format."
