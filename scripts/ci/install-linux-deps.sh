#!/usr/bin/env bash
set -euo pipefail

# WebKit's development package supplies GTK, Soup and JavaScriptCore transitively.
sudo apt-get -o Acquire::Retries=3 update
sudo apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
  build-essential pkg-config clang cmake libssl-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev libnm-dev patchelf xdg-utils file
