#!/bin/bash

# Configuration
APP_NAME="Antigravity Tools"
VERSION=$(grep '"version":' package.json | head -n 1 | awk -F: '{ print $2 }' | sed 's/[", ]//g')
DMG_NAME="Antigravity_Tools_${VERSION}_ManualFix.dmg"
SRC_APP_PATH="src-tauri/target/release/bundle/macos/${APP_NAME}.app"
DIST_DIR="dist_dmg"

echo "📦 Starting DMG packaging (with fix script)..."
echo "Version: $VERSION"

# 1. Check whether the build exists
if [ ! -d "$SRC_APP_PATH" ]; then
    echo "❌ Error: built App not found."
    echo "Please run first: npm run tauri build"
    exit 1
fi

# 2. Prepare the temp directory
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

# 3. Copy files
echo "Checking source app..."
cp -R "$SRC_APP_PATH" "$DIST_DIR/"
echo "Copying fix script..."
cp "scripts/Fix_Damaged.command" "$DIST_DIR/"
chmod +x "$DIST_DIR/Fix_Damaged.command"

# 4. Create an /Applications symlink
ln -s /Applications "$DIST_DIR/Applications"

# 5. Package the DMG
echo "Creating DMG..."
rm -f "$DMG_NAME"
hdiutil create -volname "${APP_NAME}" -srcfolder "$DIST_DIR" -ov -format UDZO "$DMG_NAME"

# 6. Clean up
rm -rf "$DIST_DIR"

echo "✅ Packaging complete!"
echo "File location: $PWD/$DMG_NAME"
