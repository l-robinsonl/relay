#!/usr/bin/env bash
# Installs a user-level desktop entry for a locally built Relay.
#
# Why this exists: on Wayland an application cannot set its own window
# icon. There is no protocol for it — unlike X11's _NET_WM_ICON, the
# compositor works out the icon by matching the window's app_id against an
# installed .desktop file. A build run straight from `target/` installs
# nothing, so the compositor finds no match and falls back to a generic
# icon in the titlebar and task switcher.
#
# Installing the .deb or .rpm does this properly and system-wide. This is
# for development, and needs no root.
#
#   ./scripts/install-desktop-entry.sh            # install
#   ./scripts/install-desktop-entry.sh --remove   # undo
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor"
ENTRY="$APPS/relay-desktop.desktop"

refresh () {
  command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$ICONS" 2>/dev/null || true
}

if [ "${1:-}" = "--remove" ]; then
  rm -f "$ENTRY"
  for size in 32x32 128x128 256x256; do
    rm -f "$ICONS/$size/apps/relay-desktop.png"
  done
  refresh
  echo "Removed the user-level desktop entry."
  exit 0
fi

BINARY="$ROOT/target/release/relay-desktop"
if [ ! -x "$BINARY" ]; then
  BINARY="$ROOT/target/debug/relay-desktop"
fi
if [ ! -x "$BINARY" ]; then
  echo "No Relay binary found. Run 'npm run tauri build' first." >&2
  exit 1
fi

mkdir -p "$APPS"
for size in 32x32 128x128 256x256; do
  mkdir -p "$ICONS/$size/apps"
done

cp "$ROOT/apps/desktop/src-tauri/icons/32x32.png"      "$ICONS/32x32/apps/relay-desktop.png"
cp "$ROOT/apps/desktop/src-tauri/icons/128x128.png"    "$ICONS/128x128/apps/relay-desktop.png"
cp "$ROOT/apps/desktop/src-tauri/icons/128x128@2x.png" "$ICONS/256x256/apps/relay-desktop.png"

# StartupWMClass must equal the window's app_id, which GTK takes from the
# binary's name. Rename the binary and this stops matching.
cat > "$ENTRY" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Relay
Comment=A fast, local-first API client
Exec=$BINARY
Icon=relay-desktop
Terminal=false
Categories=Development;
StartupWMClass=relay-desktop
StartupNotify=true
DESKTOP

refresh

echo "Installed $ENTRY"
echo "  pointing at $BINARY"
echo
echo "A compositor may cache the old icon until the window is reopened."
echo "Remove it again with: $0 --remove"
