#!/bin/bash

APP_PATH="/Applications/Antigravity Tools.app"

echo "🛠️  Fixing the 'Antigravity Tools' damaged issue..."

if [ -d "$APP_PATH" ]; then
    echo "📍 Found the app: $APP_PATH"
    echo "🔑 Administrator privileges are required to remove the quarantine attribute..."

    sudo xattr -rd com.apple.quarantine "$APP_PATH"

    if [ $? -eq 0 ]; then
        echo "✅ Fix successful! You should now be able to open the app normally."
    else
        echo "❌ Fix failed. Please check that your password is correct or that you have permission."
    fi
else
    echo "⚠️  App not found. Please confirm the app is installed under the '/Applications' directory."
    echo "   If it's installed elsewhere, run manually: sudo xattr -rd com.apple.quarantine /path/to/app"
fi
