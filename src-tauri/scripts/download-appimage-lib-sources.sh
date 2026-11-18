#!/bin/bash
# Download the source of every Ubuntu package whose libraries the AppImage
# bundles, at the exact version bundled, into one tar for the release.
#
# The AppImage bundles WebKitGTK, GTK, GLib, GStreamer and the other system
# libraries from the Ubuntu packages of the build runner, several under the
# LGPL, which asks for their source to go with the binaries. The release
# carries that source as one asset beside the AppImage, named in
# usr/share/doc/BUNDLED-LIBRARIES.txt of the AppImage and in
# THIRD-PARTY-NOTICES.md.
#
# Input: BUNDLED-LIBRARIES.txt (written by bundle-appimage-lib-notices.sh,
# "<path> <package> <version>" per library). For each package the source
# package and its version come from the dpkg database; the binary package must
# still be installed at the listed version. Each source package is fetched
# with apt-get source --download-only from the deb-src entries of the
# runner's Ubuntu archive, and, when that archive no longer lists the version
# (a package the runner image preinstalled that has an update since), from
# the Launchpad copy of the Ubuntu archive, which keeps every version, each
# file checked against the SHA-256 of its .dsc. A package whose source cannot
# be fetched at its version fails the run and is named: nothing is skipped.
#
# The tar holds sources/<source package>_<version>/ with the .dsc and its
# files, and SOURCES.txt mapping each bundled package to its source.
#
# AEROFTP_DPKG_ADMINDIR replaces /var/lib/dpkg, for the tests only.

set -euo pipefail

LIST="${1:?usage: download-appimage-lib-sources.sh <BUNDLED-LIBRARIES.txt> <out.tar>}"
OUT="${2:?usage: download-appimage-lib-sources.sh <BUNDLED-LIBRARIES.txt> <out.tar>}"
ADMINDIR="${AEROFTP_DPKG_ADMINDIR:-/var/lib/dpkg}"
LAUNCHPAD="https://launchpad.net/ubuntu/+archive/primary/+sourcefiles"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/sources"

# The packages of the list: every line after the header that names a path,
# a package and a version.
if ! packages="$(awk 'NF == 3 && $1 ~ /^usr\// { print $2 "\t" $3 }' "$LIST" | LC_ALL=C sort -u)" || [ -z "$packages" ]; then
    echo "ERROR: $LIST names no bundled package" >&2
    exit 1
fi

missing=()
declare -A SOURCE=()
index=()
while IFS=$'\t' read -r pkg version; do
    if ! line="$(dpkg-query --admindir="$ADMINDIR" -W -f='${Version}\t${source:Package}\t${source:Version}\n' "$pkg" 2>/dev/null | awk 'NR == 1')" || [ -z "$line" ]; then
        missing+=("$pkg $version: not in the dpkg database of $ADMINDIR")
        continue
    fi
    IFS=$'\t' read -r installed src srcver <<< "$line"
    if [ "$installed" != "$version" ]; then
        missing+=("$pkg: the list names $version, $installed is installed")
        continue
    fi
    SOURCE["$src"$'\t'"$srcver"]=1
    index+=("$pkg $version $src $srcver")
done <<< "$packages"

# A .dsc and its files from Launchpad, each checked against the .dsc.
from_launchpad() {
    local src="$1" ver="$2" dir="$3" plain dsc name sum
    plain="${ver#*:}"
    dsc="${src}_${plain}.dsc"
    curl -fsSL --retry 3 -o "$dir/$dsc" "$LAUNCHPAD/$src/$ver/$dsc" || return 1
    while read -r sum _ name; do
        curl -fsSL --retry 3 -o "$dir/$name" "$LAUNCHPAD/$src/$ver/$name" || return 1
        echo "$sum  $dir/$name" | sha256sum -c --quiet - || return 1
    done < <(awk '/^Checksums-Sha256:/ { on = 1; next } /^[^ ]/ { on = 0 } on && NF == 3 { print }' "$dir/$dsc")
    [ -n "$(awk '/^Checksums-Sha256:/ { on = 1; next } /^[^ ]/ { on = 0 } on && NF == 3' "$dir/$dsc")" ]
}

fetch() {
    local src="$1" ver="$2" dir="$3"
    (cd "$dir" && apt-get source --download-only -qq "$src=$ver" >/dev/null 2>&1) && return 0
    rm -f -- "$dir"/*
    from_launchpad "$src" "$ver" "$dir"
}

for key in "${!SOURCE[@]}"; do
    IFS=$'\t' read -r src srcver <<< "$key"
    dir="$work/sources/${src}_${srcver#*:}"
    mkdir -p "$dir"
    if ! fetch "$src" "$srcver" "$dir" || [ -z "$(find "$dir" -maxdepth 1 -name '*.dsc' -print -quit)" ]; then
        missing+=("$src $srcver: its source could not be downloaded")
    fi
done

if [ "${#missing[@]}" -gt 0 ]; then
    echo "ERROR: the AppImage would bundle libraries without their source:" >&2
    printf '  %s\n' "${missing[@]}" | LC_ALL=C sort >&2
    exit 1
fi

{
    echo "The source of every Ubuntu package whose libraries the AppImage bundles, at the version bundled."
    echo "Each line: bundled package, its version, its source package and version, in sources/<source>_<version>/."
    echo
    printf '%s\n' "${index[@]}" | LC_ALL=C sort
} > "$work/SOURCES.txt"
tar -C "$work" --sort=name --owner=0 --group=0 --numeric-owner -cf "$OUT" SOURCES.txt sources
echo "Downloaded the source of ${#SOURCE[@]} source packages for $(printf '%s\n' "${index[@]}" | wc -l) bundled packages: $(du -h "$OUT" | cut -f1)"
