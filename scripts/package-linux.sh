#!/usr/bin/env bash
# Package the Linux release binary as a folder with a desktop launcher, icon and installer.
# Usage: scripts/package-linux.sh [path/to/ficsv] [out-dir] [folder-name]
#   → <out-dir>/<folder-name>/{ficsv, ficsv.desktop, ficsv.png, install.sh, uninstall.sh}
set -euo pipefail
BIN="${1:-target/release/ficsv}"
OUT="${2:-dist}"
NAME="${3:-ficsv-linux}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$OUT/$NAME"
rm -rf "$DIR"
mkdir -p "$DIR"
install -m 755 "$BIN" "$DIR/ficsv"
cp "$ROOT/assets/icon/icon-256.png" "$DIR/ficsv.png"

cat > "$DIR/ficsv.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=FI CSV
GenericName=CSV Editor
Comment=Fast editor for very large CSV and TSV files
Exec=ficsv %F
Icon=ficsv
Terminal=false
Categories=Office;Spreadsheet;Utility;
MimeType=text/csv;text/tab-separated-values;text/plain;
StartupWMClass=ficsv
DESKTOP

cat > "$DIR/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Install FI CSV for the current user (no root needed):
#   ~/.local/bin/ficsv, a menu entry and the icon.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$BIN_DIR" "$DATA/applications" "$DATA/icons/hicolor/256x256/apps"
install -m 755 "$HERE/ficsv" "$BIN_DIR/ficsv"
install -m 644 "$HERE/ficsv.png" "$DATA/icons/hicolor/256x256/apps/ficsv.png"
sed "s|^Exec=ficsv|Exec=\"$BIN_DIR/ficsv\"|" "$HERE/ficsv.desktop" > "$DATA/applications/ficsv.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$DATA/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$DATA/icons/hicolor" 2>/dev/null || true
echo "Installed FI CSV to $BIN_DIR/ficsv"
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) echo "Note: $BIN_DIR is not on your PATH (the menu entry works anyway)." ;; esac
INSTALL

cat > "$DIR/uninstall.sh" <<'UNINSTALL'
#!/usr/bin/env bash
set -euo pipefail
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
rm -f "$BIN_DIR/ficsv" "$DATA/applications/ficsv.desktop" "$DATA/icons/hicolor/256x256/apps/ficsv.png"
echo "Removed FI CSV (settings in ~/.config/ficsv were kept)"
UNINSTALL
chmod 755 "$DIR/install.sh" "$DIR/uninstall.sh"
echo "$DIR"
