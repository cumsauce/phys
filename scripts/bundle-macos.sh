#!/bin/sh
# Build phys and wrap it as target/phys.app, a double-clickable macOS app.
#
#   scripts/bundle-macos.sh            build the bundle
#   scripts/bundle-macos.sh --install  also copy it to /Applications
#
# The app is not signed: the first time, right-click it and choose Open.
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p phys_app
app=target/phys.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/phys "$app/Contents/MacOS/phys"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>phys</string>
  <key>CFBundleDisplayName</key><string>phys</string>
  <key>CFBundleIdentifier</key><string>app.phys.editor</string>
  <key>CFBundleExecutable</key><string>phys</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
</dict>
</plist>
PLIST
echo "built $app"
if [ "${1:-}" = "--install" ]; then
  rm -rf /Applications/phys.app
  cp -R "$app" /Applications/phys.app
  echo "installed /Applications/phys.app"
fi
