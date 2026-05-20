#!/bin/bash
# Patch an extracted AppImage AppDir to use the AeroFTP dispatcher layout.

set -euo pipefail

APPDIR="${1:?usage: patch-appimage-dispatch.sh <AppDir> <repo-root>}"
REPO_ROOT="${2:?usage: patch-appimage-dispatch.sh <AppDir> <repo-root>}"

BIN_DIR="$APPDIR/usr/bin"
LIB_DIR="$APPDIR/usr/lib/aeroftp"
GUI_SRC="$BIN_DIR/aeroftp"
CLI_SRC="$REPO_ROOT/src-tauri/target/release/aeroftp-dispatch-bundle/aeroftp-cli"
DISPATCH_SRC="$REPO_ROOT/src-tauri/target/release/aeroftp-dispatch-bundle/aeroftp-dispatch"

if [ ! -x "$GUI_SRC" ]; then
    echo "ERROR: AppImage GUI payload not found at $GUI_SRC" >&2
    exit 1
fi
if [ ! -x "$CLI_SRC" ]; then
    echo "ERROR: CLI payload not found at $CLI_SRC" >&2
    exit 1
fi
if [ ! -x "$DISPATCH_SRC" ]; then
    echo "ERROR: dispatcher payload not found at $DISPATCH_SRC" >&2
    exit 1
fi

mkdir -p "$LIB_DIR"
if cmp -s "$GUI_SRC" "$DISPATCH_SRC"; then
    if [ ! -x "$LIB_DIR/aeroftp.bin" ]; then
        echo "ERROR: AppImage already uses dispatcher at usr/bin/aeroftp but usr/lib/aeroftp/aeroftp.bin is missing" >&2
        exit 1
    fi
else
    mv -f "$GUI_SRC" "$LIB_DIR/aeroftp.bin"
fi
install -m 0755 "$DISPATCH_SRC" "$BIN_DIR/aeroftp"
install -m 0755 "$CLI_SRC" "$LIB_DIR/aeroftp-cli"
install -m 0755 "$DISPATCH_SRC" "$LIB_DIR/aeroftp-dispatch"
rm -f \
    "$BIN_DIR/aeroftp-dispatch" \
    "$BIN_DIR/aerorsync_serve" \
    "$BIN_DIR/seed_test_profiles" \
    "$BIN_DIR/seed_axpbuntu_lab"
ln -sfn aeroftp "$BIN_DIR/aftp"
ln -sfn aeroftp "$BIN_DIR/aeroftp-cli"

find "$APPDIR" "$APPDIR/usr/share/applications" -maxdepth 1 -name '*.desktop' -print0 2>/dev/null |
while IFS= read -r -d '' desktop; do
    if grep -q '^Exec=' "$desktop"; then
        sed -i 's|^Exec=.*|Exec=aeroftp %U|' "$desktop"
    else
        echo 'Exec=aeroftp %U' >> "$desktop"
    fi
done

echo "AppImage dispatcher layout installed:"
echo "  usr/bin/aeroftp -> dispatcher"
echo "  usr/bin/aftp -> aeroftp"
echo "  usr/bin/aeroftp-cli -> aeroftp"
echo "  usr/lib/aeroftp/aeroftp.bin"
echo "  usr/lib/aeroftp/aeroftp-cli"
