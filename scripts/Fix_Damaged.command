#!/bin/bash

# Get the directory this script is located in
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
APP_PATH="$DIR/Antigravity Tools.app"

# Define colors
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

echo -e "${GREEN}==============================================${NC}"
echo -e "${GREEN}   Antigravity Tools - Quick Fix Assistant${NC}"
echo -e "${GREEN}==============================================${NC}"
echo ""

if [ -d "$APP_PATH" ]; then
    echo "📍 Attempting to fix the app: $APP_PATH"
    echo "🔑 Please enter your login password to grant permission (it will not be shown as you type)..."
    echo ""

    # Attempt to remove the quarantine attribute
    sudo xattr -rd com.apple.quarantine "$APP_PATH"

    if [ $? -eq 0 ]; then
        echo ""
        echo -e "${GREEN}✅ Fix successful!${NC}"
        echo "You can now open the app as usual."

        # Attempt to show a success notification via AppleScript
        osascript -e 'display notification "Fix successful, the app can now be opened" with title "Antigravity Tools" sound name "Glass"'
    else
        echo ""
        echo -e "${RED}❌ Fix failed${NC}"
        echo "Please check that you entered the password correctly, or try again later."
    fi
else
    echo -e "${RED}⚠️  App file not found${NC}"
    echo "Please make sure this fix script and 'Antigravity Tools.app' are in the same folder (usually /Applications)."
fi

echo ""
echo "Press any key to exit..."
read -n 1 -s -r -p ""
