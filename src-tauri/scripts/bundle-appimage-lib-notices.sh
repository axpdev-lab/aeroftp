#!/bin/bash
# Ship the copyright file of every system library linuxdeploy bundled in an
# extracted AppImage AppDir (WebKitGTK, GTK, GLib, GStreamer, ...).
#
# THIRD-PARTY-NOTICES.md covers the Cargo and npm dependencies only. The
# libraries the AppImage bundles come from the Ubuntu packages of the build
# runner, and each package ships its terms in /usr/share/doc/<package>/
# copyright. For every bundled shared object this script finds the package
# that installed it and copies that file to usr/share/doc/<package>/copyright
# in the AppDir, and writes usr/share/doc/BUNDLED-LIBRARIES.txt naming the
# package and version of each library, the source package to obtain for it.
#
# It does not guess: a bundled library whose package cannot be found, or whose
# package ships no copyright file, fails the build and is named.
#
# Run on the system the libraries were bundled from, after the AppDir is
# final (patch-appimage-lib-fallback.sh moves them to usr/lib/fallback).
# AEROFTP_LIB_OWNERS (a file of "<library file name> <package>" lines) and
# AEROFTP_DOC_ROOT replace dpkg and /usr/share/doc, for the tests only.

set -euo pipefail

APPDIR="${1:?usage: bundle-appimage-lib-notices.sh <AppDir>}"
DOC_ROOT="${AEROFTP_DOC_ROOT:-/usr/share/doc}"
OUT_DOC="$APPDIR/usr/share/doc"
LIST="$OUT_DOC/BUNDLED-LIBRARIES.txt"

# The package that installed each library file name, read in one pass over
# the file lists of the dpkg database (dpkg -S with one pattern per library
# scans that database once per pattern: 80 s for the AppImage's libraries).
declare -A OWNER=()
read_owners() {
    local name pkg
    if [ -n "${AEROFTP_LIB_OWNERS:-}" ]; then
        while read -r name pkg; do
            if [ -n "$name" ]; then OWNER["$name"]="$pkg"; fi
        done < "$AEROFTP_LIB_OWNERS"
        return 0
    fi
    while read -r name pkg; do
        OWNER["$name"]="$pkg"
    done < <(printf '%s\n' "$@" | awk '
        NR == FNR { want[$0] = 1; next }
        # Only the library folders linuxdeploy copies from: an app under /opt
        # that carries its own copy of a library is not where it came from.
        /^(\/usr)?\/lib(32|64|x32)?\// {
            n = $0; sub(/.*\//, "", n)
            if ((n in want) && !(n in seen)) {
                f = FILENAME; sub(/.*\//, "", f); sub(/\.list$/, "", f); sub(/:.*/, "", f)
                seen[n] = f; print n, f
            }
        }' - "${DPKG_INFO:-/var/lib/dpkg/info}"/*.list)
}

version_of() {
    if [ -n "${AEROFTP_LIB_OWNERS:-}" ]; then
        echo "test"
        return 0
    fi
    dpkg-query -W -f='${Version}' "$1" 2>/dev/null || echo "unknown"
}

# Every shared object under usr/lib except AeroFTP's own folder.
mapfile -t LIBS < <(find "$APPDIR/usr/lib" -path "$APPDIR/usr/lib/aeroftp" -prune -o \( -type f -o -type l \) -name '*.so*' -print | LC_ALL=C sort)
if [ "${#LIBS[@]}" -eq 0 ]; then
    echo "ERROR: no bundled libraries under $APPDIR/usr/lib" >&2
    exit 1
fi

names=()
for lib in "${LIBS[@]}"; do names+=("$(basename "$lib")"); done
read_owners "${names[@]}"

mkdir -p "$OUT_DOC"
missing=()
declare -A lines=()
for lib in "${LIBS[@]}"; do
    name="$(basename "$lib")"
    pkg="${OWNER[$name]:-}"
    if [ -z "$pkg" ]; then
        missing+=("$name: no package installed it")
        continue
    fi
    src="$DOC_ROOT/$pkg/copyright"
    if [ ! -f "$src" ]; then
        missing+=("$name: package $pkg ships no $src")
        continue
    fi
    mkdir -p "$OUT_DOC/$pkg"
    cp -L -- "$src" "$OUT_DOC/$pkg/copyright"
    chmod 644 "$OUT_DOC/$pkg/copyright"
    lines["${lib#"$APPDIR"/}"]="$pkg $(version_of "$pkg")"
done

if [ "${#missing[@]}" -gt 0 ]; then
    echo "ERROR: the AppImage would bundle libraries without their copyright file:" >&2
    printf '  %s\n' "${missing[@]}" >&2
    exit 1
fi

{
    echo "Libraries this AppImage bundles, the Ubuntu package that provides each, and its version."
    echo "The terms of each package are in usr/share/doc/<package>/copyright; its source is the Ubuntu source package of that version."
    echo
    while IFS= read -r path; do
        echo "$path ${lines[$path]}"
    done < <(printf '%s\n' "${!lines[@]}" | LC_ALL=C sort)
} > "$LIST"
chmod 644 "$LIST"
echo "Copied the copyright files of $(printf '%s\n' "${lines[@]}" | cut -d' ' -f1 | sort -u | wc -l) packages for ${#LIBS[@]} bundled libraries"
