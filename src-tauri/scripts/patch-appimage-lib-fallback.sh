#!/bin/bash
# Keep the shared libraries linuxdeploy bundled in an extracted AppImage AppDir
# as a fallback, used only on a system that lacks a library AeroFTP needs.
#
# The bundled libraries (WebKitGTK, GTK, GLib, ...) are built on Ubuntu 22.04.
# On rolling distros they conflict with the system EGL/Mesa stack, so the
# system copies must win wherever they exist (issue #90). Where the system has
# no WebKitGTK 4.1 at all, the bundled stack lets the app start instead of
# failing with "error while loading shared libraries".
#
# AppRun.wrapped (linuxdeploy) always puts usr/lib first in LD_LIBRARY_PATH, so
# the libraries move out of it, to usr/lib/fallback, and an AppRun hook adds
# that folder only when the system cannot resolve every library of the binary.

set -euo pipefail

APPDIR="${1:?usage: patch-appimage-lib-fallback.sh <AppDir>}"

FALLBACK_DIR="$APPDIR/usr/lib/fallback"
HOOK_NAME="aeroftp-lib-fallback.sh"
HOOK="$APPDIR/apprun-hooks/$HOOK_NAME"
APPRUN="$APPDIR/AppRun"
GUI_BIN="$APPDIR/usr/lib/aeroftp/aeroftp.bin"

if [ ! -x "$GUI_BIN" ]; then
    echo "ERROR: $GUI_BIN not found: run patch-appimage-dispatch.sh first" >&2
    exit 1
fi
if ! grep -q '^exec "$this_dir"/AppRun.wrapped "$@"$' "$APPRUN"; then
    echo "ERROR: $APPRUN does not end with the linuxdeploy exec line this script hooks into" >&2
    exit 1
fi

mkdir -p "$FALLBACK_DIR"
shopt -s nullglob
LIBS=("$APPDIR"/usr/lib/lib*.so*)
shopt -u nullglob
if [ "${#LIBS[@]}" -eq 0 ]; then
    echo "ERROR: no bundled libraries in $APPDIR/usr/lib" >&2
    exit 1
fi
mv "${LIBS[@]}" "$FALLBACK_DIR/"

# The system Mesa EGL/GBM stack, which an AppImage never bundles, loads these.
# A bundled copy first in LD_LIBRARY_PATH shadows the newer system one and
# WebKit's web process aborts with "Could not create default EGL display:
# EGL_BAD_PARAMETER" (issue #90, reproduced on Arch without WebKitGTK). Being
# dependencies of the system Mesa, they are on every system the fallback can
# run on, so it never needs them.
MESA_DEPS=(
    libelf.so.1
    libffi.so.8
    libwayland-client.so.0
    libXau.so.6
    libxcb-randr.so.0
    libxcb-shm.so.0
    libXdmcp.so.6
    libXext.so.6
    libzstd.so.1
)
for soname in "${MESA_DEPS[@]}"; do
    rm -f "$FALLBACK_DIR/$soname"*
done
# libacl stays a system library (every base system has it, coreutils needs
# it): scripts/assert-linux-acl-packaging.sh fails an AppImage that bundles it.
rm -f "$FALLBACK_DIR"/libacl.so*
LIBS=("$FALLBACK_DIR"/*)
if [ ! -e "$FALLBACK_DIR/libwebkit2gtk-4.1.so.0" ]; then
    echo "ERROR: linuxdeploy bundled no libwebkit2gtk-4.1.so.0, the library the fallback exists for" >&2
    exit 1
fi

# The hook is all or nothing: one missing library puts the whole fallback,
# WebKitGTK and GTK included, ahead of the system copies. That never happens
# on a system with WebKitGTK 4.1: every library aeroftp.bin links (and
# aeroftp-cli links a subset of them) is WebKitGTK 4.1 itself, one of its
# dependencies or part of the base system. Measured with ldd on minimal
# webkit2gtk-4.1 installs of Ubuntu 22.04 and 24.04, Arch and Fedora: no
# library missing. The tray libraries are loaded with dlopen at runtime and
# never trigger it.
mkdir -p "$(dirname "$HOOK")"
cat > "$HOOK" <<'EOF'
#! /usr/bin/env bash

# AeroFTP: use the libraries bundled in usr/lib/fallback only when the system
# lacks one the binary needs. System copies win wherever they exist (issue #90).
if LC_ALL=C ldd "$this_dir/usr/lib/aeroftp/aeroftp.bin" 2> /dev/null | grep -q 'not found'; then
    export LD_LIBRARY_PATH="$this_dir/usr/lib/fallback${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
EOF

sed -i "s|^exec \"\$this_dir\"/AppRun.wrapped \"\$@\"\$|source \"\$this_dir\"/apprun-hooks/\"$HOOK_NAME\"\n\n&|" "$APPRUN"
if ! grep -qF "apprun-hooks/\"$HOOK_NAME\"" "$APPRUN"; then
    echo "ERROR: could not add the $HOOK_NAME hook to $APPRUN" >&2
    exit 1
fi

echo "Bundled libraries kept as fallback: ${#LIBS[@]} entries in usr/lib/fallback"
