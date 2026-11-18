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
# The package is the one that installed the file the library was copied from.
# A file name installed by one package is that package's. A name several
# packages install is resolved by its path: the system path that ends with the
# library's path in the AppDir (a plugin folder), then the path the dynamic
# loader resolves for the name (ldconfig -p), which is where linuxdeploy
# copied it from. linuxdeploy rewrites the RPATH of what it bundles, so the
# bytes cannot be compared with the system file.
#
# It does not guess: a bundled library whose package cannot be found, or is
# not one package, or whose package ships no copyright file or has no version,
# fails the build and is named; so does a folder of the AppDir that cannot be
# listed.
#
# Run on the system the libraries were bundled from, after the AppDir is
# final (patch-appimage-lib-fallback.sh moves them to usr/lib/fallback).
# AEROFTP_DPKG_ADMINDIR, AEROFTP_LDCONFIG_P (a file with the output of
# ldconfig -p) and AEROFTP_DOC_ROOT replace /var/lib/dpkg, ldconfig and
# /usr/share/doc, for the tests only.

set -euo pipefail

APPDIR="${1:?usage: bundle-appimage-lib-notices.sh <AppDir>}"
ADMINDIR="${AEROFTP_DPKG_ADMINDIR:-/var/lib/dpkg}"
DOC_ROOT="${AEROFTP_DOC_ROOT:-/usr/share/doc}"
OUT_DOC="$APPDIR/usr/share/doc"
LIST="$OUT_DOC/BUNDLED-LIBRARIES.txt"

# Every shared object under usr/lib except AeroFTP's own folder, as a path
# relative to usr/lib. A folder find cannot read fails the build: a partial
# list would ship libraries without their terms.
if ! raw="$(cd "$APPDIR/usr/lib" && find . -path ./aeroftp -prune -o \( -type f -o -type l \) -name '*.so*' -print | LC_ALL=C sort)"; then
    echo "ERROR: could not list every library under $APPDIR/usr/lib" >&2
    exit 1
fi
LIBS=()
if [ -n "$raw" ]; then mapfile -t LIBS <<< "$raw"; fi
if [ "${#LIBS[@]}" -eq 0 ]; then
    echo "ERROR: no bundled libraries under $APPDIR/usr/lib" >&2
    exit 1
fi

if [ -n "${AEROFTP_LDCONFIG_P:-}" ]; then
    ldcache="$(cat "$AEROFTP_LDCONFIG_P")"
else
    ldcache="$(PATH="$PATH:/sbin:/usr/sbin" ldconfig -p)" || ldcache=""
fi

# One line per library: its path relative to usr/lib, then its package, or
# "-" for none and "?" followed by the candidates for more than one. One pass
# over the dpkg file lists (dpkg -S scans them once per pattern: 80 s here).
if ! owners="$(
    for rel in "${LIBS[@]}"; do
        rel="${rel#./}"
        printf '%s\t%s\t%s\n' "$rel" "${rel##*/}" "${rel#fallback/}"
    done | LDCACHE="$ldcache" awk -F'\t' '
        function norm(p) { sub(/^\/usr\//, "/", p); return p }
        function distinct(list,    n, i, parts, seen, out, k, f) {
            n = split(list, parts, "\n"); out = ""; k = 0
            for (i = 1; i <= n; i++) {
                if (parts[i] == "") continue
                split(parts[i], f, "\t")
                if (!(f[2] in seen)) { seen[f[2]] = 1; out = out (k++ ? " " : "") f[2] }
            }
            return out
        }
        BEGIN {
            n = split(ENVIRON["LDCACHE"], ll, "\n")
            for (i = 1; i <= n; i++) {
                if (ll[i] !~ / => /) continue
                nm = ll[i]; sub(/^[ \t]+/, "", nm); sub(/[ \t].*/, "", nm)
                p = ll[i]; sub(/.* => /, "", p)
                ld[nm "\t" norm(p)] = 1
            }
        }
        NR == FNR { rel[++libs] = $1; name[libs] = $2; tail[libs] = $3; want[$2] = 1; next }
        # Only the library folders linuxdeploy copies from: an app under /opt
        # that carries its own copy of a library is not where it came from.
        /^(\/usr)?\/lib(32|64|x32)?\// {
            n = $0; sub(/.*\//, "", n)
            if (n in want) {
                pk = FILENAME; sub(/.*\//, "", pk); sub(/\.list$/, "", pk); sub(/:.*/, "", pk)
                cand[n] = cand[n] norm($0) "\t" pk "\n"
            }
        }
        END {
            for (i = 1; i <= libs; i++) {
                c = cand[name[i]]
                pkgs = distinct(c)
                if (pkgs == "") { print rel[i] "\t-"; continue }
                if (pkgs !~ / /) { print rel[i] "\t" pkgs; continue }
                # The system path that ends with the path in the AppDir.
                m = split(c, parts, "\n"); byPath = ""
                for (j = 1; j <= m; j++) {
                    split(parts[j], f, "\t")
                    if (parts[j] != "" && tail[i] ~ /\// && substr(f[1], length(f[1]) - length(tail[i])) == "/" tail[i]) byPath = byPath parts[j] "\n"
                }
                p = distinct(byPath)
                if (p != "" && p !~ / /) { print rel[i] "\t" p; continue }
                # The path the dynamic loader resolves for the name.
                byLd = ""
                for (j = 1; j <= m; j++) {
                    split(parts[j], f, "\t")
                    if (parts[j] != "" && ((name[i] "\t" f[1]) in ld)) byLd = byLd parts[j] "\n"
                }
                p = distinct(byLd)
                if (p != "" && p !~ / /) { print rel[i] "\t" p; continue }
                print rel[i] "\t? " pkgs
            }
        }' - "$ADMINDIR"/info/*.list
)"; then
    echo "ERROR: could not read the owners of the libraries from $ADMINDIR" >&2
    exit 1
fi
declare -A OWNER=()
while IFS=$'\t' read -r rel pkg; do
    OWNER["$rel"]="$pkg"
done <<< "$owners"

version_of() {
    dpkg-query --admindir="$ADMINDIR" -W -f='${Version}\n' "$1" 2>/dev/null | awk 'NR == 1' || true
}

mkdir -p "$OUT_DOC"
missing=()
declare -A lines=()
for rel in "${LIBS[@]}"; do
    rel="${rel#./}"
    pkg="${OWNER[$rel]:--}"
    if [ "$pkg" = "-" ]; then
        missing+=("$rel: no package installed it")
        continue
    fi
    if [ "${pkg:0:1}" = "?" ]; then
        missing+=("$rel: installed under that name by more than one package (${pkg#\? }), and neither its path nor ldconfig names one")
        continue
    fi
    src="$DOC_ROOT/$pkg/copyright"
    if [ ! -f "$src" ]; then
        missing+=("$rel: package $pkg ships no $src")
        continue
    fi
    version="$(version_of "$pkg")"
    if [ -z "$version" ]; then
        missing+=("$rel: package $pkg has no version in $ADMINDIR")
        continue
    fi
    mkdir -p "$OUT_DOC/$pkg"
    cp -L -- "$src" "$OUT_DOC/$pkg/copyright"
    chmod 644 "$OUT_DOC/$pkg/copyright"
    lines["usr/lib/$rel"]="$pkg $version"
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
