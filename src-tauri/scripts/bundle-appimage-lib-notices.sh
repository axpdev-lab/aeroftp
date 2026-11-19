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
# package and version of each library and the Ubuntu source package and
# version it was built from, the Launchpad page of that source version
# (https://launchpad.net/ubuntu/+source/<source>/<version>, the version with
# its epoch, as Launchpad names it), and the written offer of that source.
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
# not one package, or is one package that does not own the file the loader
# resolves the name to, or whose package ships no copyright file or has no version
# or source version,
# fails the build and is named; so does a folder of the AppDir that cannot be
# listed.
#
# Run on the system the libraries were bundled from, after the AppDir is
# final (patch-appimage-lib-fallback.sh moves them to usr/lib/fallback).
# Every source version listed must be one of the Ubuntu archive, which the
# written offer names: the .dsc of that exact version is fetched from
# Launchpad's primary archive and its Source and Version must be the ones
# dpkg gives. A library from a PPA or another repository, or a version the
# archive does not have, fails the build and is named. Launchpad not
# answering (three tries) fails it too, never a silent pass.
#
# AEROFTP_DPKG_ADMINDIR, AEROFTP_LDCONFIG_P (a file with the output of
# ldconfig -p) and AEROFTP_DOC_ROOT replace /var/lib/dpkg, ldconfig and
# /usr/share/doc, for the tests only; so do AEROFTP_DSC_FETCH (a command run
# as `<command> <url> <file>` that writes the body to <file> and prints the
# HTTP status) and AEROFTP_DSC_BACKOFF (seconds, 5 by default) for the fetch.

set -euo pipefail

APPDIR="${1:?usage: bundle-appimage-lib-notices.sh <AppDir> <AeroFTP version>}"
VERSION="${2:?usage: bundle-appimage-lib-notices.sh <AppDir> <AeroFTP version>}"
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
                ldname[nm] = 1
                ldpath[nm] = ldpath[nm] (ldpath[nm] == "" ? "" : " ") p
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
                m = split(c, parts, "\n")
                # One package installed the name. A library the loader
                # resolves (not a plugin in its own folder) came from the
                # path ldconfig gives: the package must own that file.
                if (pkgs !~ / /) {
                    if (tail[i] ~ /\// || !(name[i] in ldname)) { print rel[i] "\t" pkgs; continue }
                    own = 0
                    for (j = 1; j <= m; j++) {
                        split(parts[j], f, "\t")
                        if (parts[j] != "" && ((name[i] "\t" f[1]) in ld)) own = 1
                    }
                    print rel[i] "\t" (own ? pkgs : "!" pkgs " " ldpath[name[i]])
                    continue
                }
                # The system path that ends with the path in the AppDir.
                byPath = ""
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

# "<version> <source package> <source version> <Launchpad page of that
# source version>", empty when any is missing.
version_of() {
    dpkg-query --admindir="$ADMINDIR" -W -f='${Version}\t${source:Package}\t${source:Version}\n' "$1" 2>/dev/null |
        awk -F'\t' 'NR == 1 && $1 != "" && $2 != "" && $3 != "" { print $1 " " $2 " " $3 " https://launchpad.net/ubuntu/+source/" $2 "/" $3 }' || true
}

# The .dsc of a source version at Launchpad, into the file $3. Returns 0
# when it is there, 1 when Launchpad has no such file, 2 when Launchpad did
# not answer three times.
fetch_dsc() {
    local url="$1" out="$2" code tries=0
    while :; do
        tries=$((tries + 1))
        if [ -n "${AEROFTP_DSC_FETCH:-}" ]; then
            code="$("$AEROFTP_DSC_FETCH" "$url" "$out")" || code=000
        else
            code="$(curl -sSL --max-time 60 -o "$out" -w '%{http_code}' "$url")" || code=000
        fi
        case "$code" in
            200) return 0 ;;
            404 | 410) return 1 ;;
        esac
        [ "$tries" -ge 3 ] && return 2
        sleep $((tries * ${AEROFTP_DSC_BACKOFF:-5}))
    done
}

# Sets DSC_PROBLEM: empty when the source version is the Ubuntu archive's,
# else why not. Each source version is fetched once (in this shell, not a
# subshell, so the cache holds).
declare -A DSC_CHECKED=()
DSC_PROBLEM=""
dsc_problem() {
    local src="$1" srcver="$2" key="$1 $2"
    if [ -n "${DSC_CHECKED[$key]+x}" ]; then
        DSC_PROBLEM="${DSC_CHECKED[$key]}"
        return
    fi
    local url="https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/$src/$srcver/${src}_${srcver#*:}.dsc"
    local body problem="" status=0
    body="$(mktemp)"
    fetch_dsc "$url" "$body" || status=$?
    if [ "$status" -eq 1 ]; then
        problem="source $src $srcver is not in the Ubuntu archive (Launchpad has no $url): a PPA, another repository, or a version the archive does not have"
    elif [ "$status" -ne 0 ]; then
        problem="Launchpad did not answer for $url after three tries; the source of $src $srcver cannot be checked, so the build stops"
    else
        local got_src got_ver
        # The field value is all after the first colon: a version keeps its
        # epoch ("2:1.7-0ubuntu1").
        got_src="$(awk 'tolower($0) ~ /^source:/ { sub(/^[^:]*:[ \t]*/, ""); print; exit }' "$body" | tr -d '\r')"
        got_ver="$(awk 'tolower($0) ~ /^version:/ { sub(/^[^:]*:[ \t]*/, ""); print; exit }' "$body" | tr -d '\r')"
        if [ "$got_src" != "$src" ] || [ "$got_ver" != "$srcver" ]; then
            problem="Launchpad's $url is source ${got_src:-?} ${got_ver:-?}, not $src $srcver"
        fi
    fi
    rm -f -- "$body"
    DSC_CHECKED[$key]="$problem"
    DSC_PROBLEM="$problem"
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
    if [ "${pkg:0:1}" = "!" ]; then
        # "!<package> <the path(s) the loader resolves the name to>"
        sole="${pkg#!}"
        missing+=("$rel: only package ${sole%% *} installs that name, but the loader resolves it (ldconfig -p) to ${sole#* }, a file that package does not own")
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
        missing+=("$rel: package $pkg has no version or source version in $ADMINDIR")
        continue
    fi
    read -r _ src_pkg src_ver _ <<< "$version"
    dsc_problem "$src_pkg" "$src_ver"
    if [ -n "$DSC_PROBLEM" ]; then
        missing+=("$rel: package $pkg: $DSC_PROBLEM")
        continue
    fi
    mkdir -p "$OUT_DOC/$pkg"
    cp -L -- "$src" "$OUT_DOC/$pkg/copyright"
    chmod 644 "$OUT_DOC/$pkg/copyright"
    lines["usr/lib/$rel"]="$pkg $version"
done

if [ "${#missing[@]}" -gt 0 ]; then
    echo "ERROR: the AppImage would bundle libraries without their copyright file, or whose source is not one of the Ubuntu archive:" >&2
    printf '  %s\n' "${missing[@]}" >&2
    exit 1
fi

# The archive snapshot of the build time (UTC) keeps every version listed
# when Launchpad no longer serves it. The time, not the midnight of that day:
# a package published earlier that day and installed for the build is not in
# the midnight snapshot. This runs after the libraries were installed and
# bundled, so the snapshot at this second already holds each of them.
SNAPSHOT_DATE="$(date -u +%Y%m%dT%H%M%SZ)"
{
    echo "Libraries this AppImage bundles. Each line: the library, the Ubuntu package that provides it and its version, then the Ubuntu source package it was built from, its version, and the Launchpad page of that source version."
    echo "The terms of each package are in usr/share/doc/<package>/copyright."
    echo "Written offer: for at least three years from the date of the AeroFTP $VERSION release, anyone who asks can obtain from us the complete corresponding source code of each library listed here, at the source package version listed, for a charge no more than our cost of providing it. Ask at dev@aeroftp.app, the contact published in SECURITY.md of https://github.com/axpdev-lab/aeroftp. Every version listed is a package of the Ubuntu archive, and the archive keeps its source: we fetch that exact version from the Launchpad page listed, or from the archive snapshot of the build date at https://snapshot.ubuntu.com/ubuntu/$SNAPSHOT_DATE/ when Launchpad no longer serves it, and send it to you."
    echo
    while IFS= read -r path; do
        echo "$path ${lines[$path]}"
    done < <(printf '%s\n' "${!lines[@]}" | LC_ALL=C sort)
} > "$LIST"
chmod 644 "$LIST"
echo "Copied the copyright files of $(printf '%s\n' "${lines[@]}" | cut -d' ' -f1 | sort -u | wc -l) packages for ${#LIBS[@]} bundled libraries"
