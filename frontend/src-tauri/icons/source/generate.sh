#!/bin/bash
# Regenerates every app, tray and in-app icon from the SVGs in this folder.
# Needs resvg (`cargo install resvg`) and the frontend's node_modules (Tauri CLI).
#
#   app.svg        full-bleed tile: Windows .ico, Linux/Android/iOS PNGs, in-app logos
#   app-macos.svg  Apple-grid tile with shadow: icon.icns only (the Dock)
#   tray-*.svg     menu-bar / tray icons, idle and recording (src/tray.rs)
#   overlay-recording.svg  Windows taskbar-button overlay while recording
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ICONS="$HERE/.."
FRONTEND="$ICONS/../.."
TAURI="$FRONTEND/node_modules/.bin/tauri"
RESVG="${RESVG:-$(command -v resvg || echo "$HOME/.cargo/bin/resvg")}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$RESVG" -w 1024 -h 1024 "$HERE/app.svg" "$WORK/app.png"
"$RESVG" -w 1024 -h 1024 "$HERE/app-macos.svg" "$WORK/app-macos.png"

"$TAURI" icon "$WORK/app.png" -o "$WORK/full" >/dev/null
"$TAURI" icon "$WORK/app-macos.png" -o "$WORK/macos" >/dev/null
cp -R "$WORK/full/." "$ICONS/"
cp "$WORK/macos/icon.icns" "$ICONS/icon.icns"

mkdir -p "$ICONS/tray"
for name in tray-macos tray-macos-recording tray-windows tray-windows-recording overlay-recording; do
  "$RESVG" "$HERE/$name.svg" "$ICONS/tray/$name.png"
done

PUBLIC="$FRONTEND/public"
"$RESVG" -w 512 -h 512 "$HERE/app.svg" "$PUBLIC/logo.png"
"$RESVG" -w 256 -h 256 "$HERE/app.svg" "$PUBLIC/logo-collapsed.png"
"$RESVG" -w 128 -h 128 "$HERE/app.svg" "$PUBLIC/icon_128x128.png"
"$RESVG" -w 64 -h 64 "$HERE/app.svg" "$PUBLIC/icon_32x32@2x.png"
echo "Icons regenerated."
