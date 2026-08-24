#!/usr/bin/env bash
#
# aur-preflight.sh - refuse to publish an AUR package that cannot be built.
#
# Run it from the aeroftp-bin checkout, or point it at one:
#
#     .github/scripts/aur-preflight.sh /path/to/aeroftp-bin
#
# Install it as the pre-push hook of that checkout so it cannot be forgotten:
#
#     ln -s /path/to/aeroftp/.github/scripts/aur-preflight.sh \
#           /path/to/aeroftp-bin/.git/hooks/pre-push
#
# (`ln -s`, not `ln -sf`: refuse rather than silently replace a hook that is
# already there and may be doing something else.)
#
# Why this exists: v3.5.2 was pushed with the sha256sums array closed after its
# first element. The PKGBUILD could not be sourced at all, so makepkg died before
# it downloaded anything and nobody could install the package for about 23 hours,
# until v3.5.3 replaced it. A single `bash -n` would have caught it. The AUR runs
# no CI: whatever is pushed is what every user gets, immediately.
#
# Checks, in order:
#   1. PKGBUILD parses as bash
#   2. .SRCINFO agrees with PKGBUILD, value by value, sources and checksums
#      included (regenerated diff on Arch, expanded compare off it)
#   3. sha256sums[0] is a full 64 hex digest, not SKIP and not a placeholder
#   4. no source line carries a version other than pkgver
#   5. every release URL resolves (skip deliberately with --offline)
#
set -uo pipefail

offline=0
repo=""
for arg in "$@"; do
    case "$arg" in
        --offline) offline=1 ;;
        -h|--help) sed -n '3,29p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *) repo="$arg" ;;
    esac
done

if [ -n "$repo" ]; then
    cd "$repo" || { echo "aur-preflight: cannot enter $repo" >&2; exit 2; }
elif root=$(git rev-parse --show-toplevel 2>/dev/null); then
    cd "$root" || exit 2
fi

[ -f PKGBUILD ] || { echo "aur-preflight: no PKGBUILD here ($PWD)" >&2; exit 2; }
[ -f .SRCINFO ] || { echo "aur-preflight: no .SRCINFO here ($PWD)" >&2; exit 2; }

fail=0
note() { echo "aur-preflight: $*" >&2; fail=1; }

# mktemp can fail (full or read-only TMPDIR). Bail out rather than carry an empty
# path into the trap below.
tmp=$(mktemp -d) || { echo "aur-preflight: cannot create a temporary directory" >&2; exit 2; }
trap 'rm -rf -- "$tmp"' EXIT

# 1. The PKGBUILD must parse. This is the v3.5.2 check.
if ! bash -n PKGBUILD 2>"$tmp/syntax"; then
    note "PKGBUILD is not sourceable, makepkg would abort before downloading anything"
    sed 's/^/    /' "$tmp/syntax" >&2
    # Every check below reads fields out of the PKGBUILD, so stop here.
    echo "aur-preflight: FAILED" >&2
    exit 1
fi

# Read the PKGBUILD the way makepkg does, in a subshell, so that ${pkgver} inside
# a source URL is compared expanded rather than as source text. Only top-level
# assignments run; prepare()/package() are defined, never called.
if ! bash --noprofile --norc -c '
        set -u
        # shellcheck disable=SC1091
        . ./PKGBUILD >/dev/null 2>&1 || exit 1
        declare -p pkgname pkgver pkgrel pkgdesc source sha256sums 2>/dev/null
    ' > "$tmp/vars" 2>/dev/null || [ ! -s "$tmp/vars" ]; then
    note "PKGBUILD parses but does not evaluate, or declares none of pkgname/pkgver/source"
    echo "aur-preflight: FAILED" >&2
    exit 1
fi
# shellcheck disable=SC1090
. "$tmp/vars"

srcfield() { sed -n "s/^[[:space:]]*$1 = //p" .SRCINFO | head -1; }
mapfile -t srcinfo_sources < <(sed -n 's/^[[:space:]]*source = //p' .SRCINFO)
mapfile -t srcinfo_sums    < <(sed -n 's/^[[:space:]]*sha256sums = //p' .SRCINFO)

# 2. .SRCINFO is what the AUR and every helper read. A PKGBUILD fix that never
#    reached it ships the old package to users while looking fixed locally.
if command -v makepkg >/dev/null 2>&1; then
    if ! makepkg --printsrcinfo > "$tmp/srcinfo" 2>"$tmp/srcinfo.err"; then
        note "makepkg --printsrcinfo failed"
        sed 's/^/    /' "$tmp/srcinfo.err" >&2
    elif ! diff -q "$tmp/srcinfo" .SRCINFO >/dev/null; then
        note ".SRCINFO does not match PKGBUILD (regenerate: makepkg --printsrcinfo > .SRCINFO)"
        diff -u .SRCINFO "$tmp/srcinfo" | sed 's/^/    /' >&2
    fi
else
    # Off Arch (.SRCINFO is hand-edited there) compare every value, not just the
    # scalars: a checksum bumped in the PKGBUILD alone leaves users on the old
    # digest, and equal entry counts hide it.
    for k in pkgname pkgver pkgrel pkgdesc; do
        p="${!k-}"; s=$(srcfield "$k")
        [ "$p" = "$s" ] || note "$k differs: PKGBUILD='$p' .SRCINFO='$s'"
    done
    if [ "${#source[@]}" -ne "${#srcinfo_sources[@]}" ]; then
        note "PKGBUILD has ${#source[@]} sources, .SRCINFO has ${#srcinfo_sources[@]}"
    else
        for i in "${!source[@]}"; do
            [ "${source[$i]}" = "${srcinfo_sources[$i]}" ] || \
                note "source[$i] differs:
    PKGBUILD: ${source[$i]}
    .SRCINFO: ${srcinfo_sources[$i]}"
        done
    fi
    if [ "${#sha256sums[@]}" -ne "${#srcinfo_sums[@]}" ]; then
        note "PKGBUILD has ${#sha256sums[@]} sha256sums, .SRCINFO has ${#srcinfo_sums[@]}"
    else
        for i in "${!sha256sums[@]}"; do
            [ "${sha256sums[$i]}" = "${srcinfo_sums[$i]}" ] || \
                note "sha256sums[$i] differs: PKGBUILD='${sha256sums[$i]}' .SRCINFO='${srcinfo_sums[$i]}'"
        done
    fi
fi

[ "${#source[@]}" -eq "${#sha256sums[@]}" ] || \
    note "PKGBUILD has ${#source[@]} sources but ${#sha256sums[@]} sha256sums"

# 3. A real digest on the payload. SKIP belongs to the Sigstore bundle only, which
#    prepare() authenticates with cosign; SKIP on the .deb would pin nothing.
#    Match the whole value: a 64 char string with one stray character is not a
#    digest, and makepkg would reject it after the user has downloaded 75 MB.
first_sum="${sha256sums[0]-}"
if [[ ! "$first_sum" =~ ^[0-9a-f]{64}$ ]]; then
    note "sha256sums[0] is '${first_sum}', expected 64 lowercase hex characters (the .deb digest)"
fi

# 4. Both source lines are expanded in .SRCINFO, so a hand edit can update one and
#    miss the other, and a URL that hardcodes a version ignores the bump entirely.
for s in "${source[@]}" "${srcinfo_sources[@]}"; do
    case "$s" in
        *"$pkgver"*) ;;
        *) note "source does not carry pkgver ${pkgver}: ${s}" ;;
    esac
done

# 5. A source that 404s fails the build for every user, and the release assets are
#    named by the workflow, so a rename or a missing upload only shows up here.
# Filtered in bash rather than through sed/grep: this runs as a git hook, where a
# missing tool must not silently produce an empty list and a green summary line.
urls=()
for s in "${source[@]}"; do
    u="${s#*::}"   # drop the "renamed-file::" prefix when there is one
    case "$u" in https://*|http://*) urls+=("$u") ;; esac
done

if [ "$offline" -eq 1 ]; then
    echo "aur-preflight: --offline, not checking that the ${#urls[@]} sources resolve" >&2
elif [ "${#urls[@]}" -eq 0 ]; then
    note "no http(s) source to check, which is not what this package looks like"
elif ! command -v curl >/dev/null 2>&1; then
    # Fail closed: silently skipping would print a green line that means nothing.
    note "curl is not available, cannot check that the sources resolve (pass --offline to accept that)"
else
    for url in "${urls[@]}"; do
        code=$(curl -sIL --retry 2 --max-time 25 -o /dev/null -w '%{http_code}' "$url" 2>/dev/null)
        [ "$code" = "200" ] || note "source does not resolve (HTTP ${code:-none}): $url"
    done
fi

if [ "$fail" -ne 0 ]; then
    echo "aur-preflight: FAILED, refusing to publish a broken package" >&2
    exit 1
fi
if [ "$offline" -eq 1 ]; then
    echo "aur-preflight: PKGBUILD sourceable, .SRCINFO in sync, sources NOT checked (--offline)"
else
    echo "aur-preflight: PKGBUILD sourceable, .SRCINFO in sync, ${#urls[@]} sources resolve"
fi
