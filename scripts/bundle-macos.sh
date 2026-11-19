#!/usr/bin/env bash
# Wrap the release binary in a minimal macOS .app bundle.
# Usage: scripts/bundle-macos.sh [path/to/ficsv] [out-dir]
set -euo pipefail
BIN="${1:-target/release/ficsv}"
OUT="${2:-dist}"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP="$OUT/ficsv.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ficsv"
cp "$(dirname "$0")/../assets/icon/ficsv.icns" "$APP/Contents/Resources/ficsv.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>ficsv</string>
  <key>CFBundleDisplayName</key><string>ficsv</string>
  <key>CFBundleIdentifier</key><string>app.ficsv</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>ficsv</string>
  <key>CFBundleIconFile</key><string>ficsv</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Delimited text</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSItemContentTypes</key>
      <array><string>public.comma-separated-values-text</string><string>public.tab-separated-values-text</string><string>public.plain-text</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
echo "Built $APP"
