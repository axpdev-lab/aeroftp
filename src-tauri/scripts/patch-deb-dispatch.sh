#!/bin/bash
# Rewrite a Tauri .deb so /usr/bin contains only dispatcher entrypoints.

set -euo pipefail

DEB="${1:?usage: patch-deb-dispatch.sh <package.deb>}"
if [ ! -f "$DEB" ]; then
    echo "ERROR: package not found: $DEB" >&2
    exit 1
fi

WORK="$(mktemp -d)"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

dpkg-deb -R "$DEB" "$WORK/pkg"

BIN_DIR="$WORK/pkg/usr/bin"
LIB_DIR="$WORK/pkg/usr/lib/aeroftp"
mkdir -p "$BIN_DIR" "$LIB_DIR"

if [ ! -x "$BIN_DIR/aeroftp" ]; then
    echo "ERROR: Tauri GUI payload not found at usr/bin/aeroftp" >&2
    exit 1
fi
if [ ! -x "$LIB_DIR/aeroftp-cli" ]; then
    echo "ERROR: CLI payload not found at usr/lib/aeroftp/aeroftp-cli" >&2
    exit 1
fi
if [ ! -x "$LIB_DIR/aeroftp-dispatch" ]; then
    echo "ERROR: dispatcher payload not found at usr/lib/aeroftp/aeroftp-dispatch" >&2
    exit 1
fi

if cmp -s "$BIN_DIR/aeroftp" "$LIB_DIR/aeroftp-dispatch"; then
    if [ ! -x "$LIB_DIR/aeroftp.bin" ]; then
        echo "ERROR: package already uses dispatcher at usr/bin/aeroftp but usr/lib/aeroftp/aeroftp.bin is missing" >&2
        exit 1
    fi
else
    mv -f "$BIN_DIR/aeroftp" "$LIB_DIR/aeroftp.bin"
fi
install -m 0755 "$LIB_DIR/aeroftp-dispatch" "$BIN_DIR/aeroftp"
rm -f \
    "$BIN_DIR/aeroftp-cli" \
    "$BIN_DIR/aeroftp-dispatch" \
    "$BIN_DIR/aerorsync_serve" \
    "$BIN_DIR/seed_test_profiles" \
    "$BIN_DIR/seed_axpbuntu_lab"
ln -sfn aeroftp "$BIN_DIR/aftp"
ln -sfn aeroftp "$BIN_DIR/aeroftp-cli"

for desktop in \
    "$WORK/pkg/usr/share/applications/AeroFTP.desktop" \
    "$WORK/pkg/usr/share/applications/com.aeroftp.AeroFTP.desktop" \
    "$WORK/pkg/usr/share/applications/aeroftp.desktop"
do
    [ -f "$desktop" ] || continue
    if grep -q '^Exec=' "$desktop"; then
        sed -i 's|^Exec=.*|Exec=/usr/bin/aeroftp %U|' "$desktop"
    else
        echo 'Exec=/usr/bin/aeroftp %U' >> "$desktop"
    fi
done

(
    cd "$WORK/pkg"
    find . -path './DEBIAN' -prune -o -type f -printf '%P\n' |
        sort |
        while IFS= read -r file; do
            md5sum "$file"
        done > DEBIAN/md5sums
)

dpkg-deb --root-owner-group -b "$WORK/pkg" "$DEB" >/dev/null
echo "Patched Debian dispatcher layout: $DEB"
