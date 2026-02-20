#!/bin/bash
# Build the AeroFTP FinderSync extension (.appex)
# This script is run on macOS CI to compile the Swift FinderSync extension
# and place it in the app bundle's PlugIns directory.
#
# Usage: ./build-finder-extension.sh [path-to-app-bundle]
# Example: ./build-finder-extension.sh target/release/bundle/macos/AeroFTP.app

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
EXTENSION_DIR="$SCRIPT_DIR/AeroFTPFinderSync"
APP_BUNDLE="${1:-}"

if [ -z "$APP_BUNDLE" ]; then
    echo "Usage: $0 <path-to-AeroFTP.app>"
    exit 1
fi

if [ ! -d "$APP_BUNDLE" ]; then
    echo "Error: App bundle not found at $APP_BUNDLE"
    exit 1
fi

echo "Building AeroFTP FinderSync extension..."

# Create build directory
BUILD_DIR="$SCRIPT_DIR/build"
mkdir -p "$BUILD_DIR"

# Compile Swift extension
swiftc \
    -target arm64-apple-macos11.0 \
    -sdk "$(xcrun --show-sdk-path)" \
    -framework Cocoa \
    -framework FinderSync \
    -emit-executable \
    -o "$BUILD_DIR/AeroFTPFinderSync" \
    "$EXTENSION_DIR/FinderSync.swift"

# Also compile for x86_64
swiftc \
    -target x86_64-apple-macos11.0 \
    -sdk "$(xcrun --show-sdk-path)" \
    -framework Cocoa \
    -framework FinderSync \
    -emit-executable \
    -o "$BUILD_DIR/AeroFTPFinderSync-x86_64" \
    "$EXTENSION_DIR/FinderSync.swift"

# Create universal binary
lipo -create \
    "$BUILD_DIR/AeroFTPFinderSync" \
    "$BUILD_DIR/AeroFTPFinderSync-x86_64" \
    -output "$BUILD_DIR/AeroFTPFinderSync-universal"

# Create .appex bundle structure
APPEX_DIR="$BUILD_DIR/AeroFTPFinderSync.appex"
rm -rf "$APPEX_DIR"
mkdir -p "$APPEX_DIR/Contents/MacOS"

cp "$BUILD_DIR/AeroFTPFinderSync-universal" "$APPEX_DIR/Contents/MacOS/AeroFTPFinderSync"
cp "$EXTENSION_DIR/Info.plist" "$APPEX_DIR/Contents/"

# Install into app bundle PlugIns
PLUGINS_DIR="$APP_BUNDLE/Contents/PlugIns"
mkdir -p "$PLUGINS_DIR"
cp -R "$APPEX_DIR" "$PLUGINS_DIR/"

echo "FinderSync extension installed at: $PLUGINS_DIR/AeroFTPFinderSync.appex"

# If codesign identity is available, sign the extension
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
    echo "Signing extension with identity: $APPLE_SIGNING_IDENTITY"
    codesign --force --deep --sign "$APPLE_SIGNING_IDENTITY" \
        --entitlements "$EXTENSION_DIR/AeroFTPFinderSync.entitlements" \
        "$PLUGINS_DIR/AeroFTPFinderSync.appex"
    echo "Extension signed successfully"
else
    echo "Warning: APPLE_SIGNING_IDENTITY not set, extension unsigned"
    echo "Set APPLE_SIGNING_IDENTITY env var for code signing"
fi

# Cleanup build artifacts
rm -rf "$BUILD_DIR"

echo "Done! FinderSync extension built and installed."
echo "Users need to enable it in System Preferences > Extensions > Finder Extensions"
