#!/bin/sh
# Installs ShiftDrop for the current user.
#
#   curl -fsSL https://raw.githubusercontent.com/jayhemnani9910/shiftdrop-rust/main/install.sh | sh
#       downloads the latest release (x86_64), no Rust needed
#   ./install.sh
#       from a checkout: builds from source (needs cargo); run it again after changing the code
#
# The binary is copied to ~/.local/bin, so `cargo clean` or a branch switch
# can't break the installed app. uninstall.sh removes exactly what this adds.
set -eu
uuid="shiftdrop@jayhemnani"
app_id="dev.jayhemnani.ShiftDrop"
repo_url="https://github.com/jayhemnani9910/shiftdrop-rust"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
bin="$HOME/.local/bin/shiftdrop"
ext="$data/gnome-shell/extensions/$uuid"

here="$(cd "$(dirname "$0")" && pwd)"
if [ -f "$here/Cargo.toml" ] && [ -d "$here/extension/$uuid" ]; then
  cargo build --release --manifest-path "$here/Cargo.toml"
  built="$here/target/release/shiftdrop"
  ext_src="$here/extension/$uuid"
else
  if [ "$(uname -m)" != x86_64 ]; then
    echo "release builds are x86_64 only; build from source: $repo_url" >&2
    exit 1
  fi
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  echo "downloading the latest release..."
  curl -fsSL "$repo_url/releases/latest/download/shiftdrop-x86_64-linux.tar.gz" | tar -xz -C "$tmp"
  built="$tmp/shiftdrop/shiftdrop"
  ext_src="$tmp/shiftdrop/extension/$uuid"
fi

mkdir -p "$HOME/.local/bin" "$data/gnome-shell/extensions" "$data/dbus-1/services" \
         "$data/applications" "$data/nautilus/scripts"

# copy next to it, then rename: the running service keeps its old file until it restarts
install -m 755 "$built" "$bin.new"
mv -f "$bin.new" "$bin"
# a checkout links its extension, so edits show up after a logout; a download gets a copy
rm -rf "$ext"
if [ -n "${tmp:-}" ]; then
  cp -r "$ext_src" "$ext"
else
  ln -s "$ext_src" "$ext"
fi

# lets the extension start the app on the first Shift-drag
cat > "$data/dbus-1/services/$app_id.service" <<SERVICE
[D-BUS Service]
Name=$app_id
Exec=$bin --gapplication-service
SERVICE

cat > "$data/applications/$app_id.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=ShiftDrop
Comment=Drag a file, hold Shift, pick a format
Exec=$bin %F
Terminal=false
NoDisplay=true
DESKTOP

# start at login, so the first drag doesn't wait for GTK to start
mkdir -p "$HOME/.config/autostart"
cat > "$HOME/.config/autostart/$app_id.desktop" <<AUTOSTART
[Desktop Entry]
Type=Application
Name=ShiftDrop
Exec=$bin --gapplication-service
NoDisplay=true
X-GNOME-Autostart-enabled=true
AUTOSTART

# Files: right-click > Scripts > ShiftDrop
ln -sfn "$bin" "$data/nautilus/scripts/ShiftDrop"

# the converters are system packages; say which are missing rather than install them
missing=""
need() { command -v "$1" >/dev/null 2>&1 || missing="$missing $2"; }
need ffmpeg ffmpeg
need magick imagemagick
need gs ghostscript
need pdftoppm poppler-utils
need soffice libreoffice-writer
if [ -n "$missing" ]; then
  echo "for every format, also run: sudo apt install$missing"
fi

gnome-extensions enable "$uuid" 2>/dev/null && echo "extension enabled" || \
  echo "log out and back in once, then run: gnome-extensions enable $uuid"
echo "installed. drag a file, hold Shift, drop it on a format."
