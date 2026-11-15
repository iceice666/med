#!/bin/sh
# Wrap the built binary in a minimal macOS .app bundle (target/<profile>/med.app).
# Usage: scripts/bundle.sh [debug|release]
set -eu
profile="${1:-debug}"
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="$root/target/$profile/med"
app="$root/target/$profile/med.app"
[ -x "$bin" ] || { echo "build first: cargo build -p med${1:+ --$profile}" >&2; exit 1; }
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/med"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>med</string>
  <key>CFBundleDisplayName</key><string>med</string>
  <key>CFBundleIdentifier</key><string>dev.med.editor</string>
  <key>CFBundleExecutable</key><string>med</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
echo "$app"
