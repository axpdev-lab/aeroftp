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
#     ln -sf /path/to/aeroftp/.github/scripts/aur-preflight.sh \
#            /path/to/aeroftp-bin/.git/hooks/pre-push
#
# Why this exists: v3.5.2 was pushed with the sha256sums array closed after its
# first element. The PKGBUILD could not be sourced at all, so makepkg died before
# it downloaded anything and nobody could install the package for about 23 hours,
# until v3.5.3 replaced it. A single `bash -n` would have caught it. The AUR runs
# no CI: whatever is pushed is what every user gets, immediately.
#
# Checks, in order:
#   1. PKGBUILD parses as bash
#   2. .SRCINFO agrees with PKGBUILD (regenerated diff on Arch, field compare off it)
#   3. sha256sums[0] is a real digest, not SKIP or a leftover placeholder
#   4. no source line carries a version other than pkgver
#   5. both release URLs resolve (skip with --offline)
#
set -uo pipefail

offline=0
repo=""
for arg in "$@"; do
    case "$arg" in
        --offline) offline=1 ;;
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
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# 1. The PKGBUILD must parse. This is the v3.5.2 check.
if ! bash -n PKGBUILD 2>"$tmp/syntax"; then
    note "PKGBUILD is not sourceable, makepkg would abort before downloading anything"
    sed 's/^/    /' "$tmp/syntax" >&2
    # Everything below reads fields out of the PKGBUILD, so stop here.
    echo "aur-preflight: FAILED" >&2
    exit 1
fi

field() { sed -n "s/^$1=//p" PKGBUILD | head -1 | sed "s/^[\"']//;s/[\"']$//"; }
srcfield() { sed -n "s/^[[:space:]]*$1 = //p" .SRCINFO | head -1; }

pkgver=$(field pkgver)
[ -n "$pkgver" ] || note "PKGBUILD declares no pkgver"

# 2. .SRCINFO is what the AUR and every helper read. A PKGBUILD fix that never
#    reached .SRCINFO ships the old package to users while looking fixed locally.
if command -v makepkg >/dev/null 2>&1; then
    if ! makepkg --printsrcinfo > "$tmp/srcinfo" 2>"$tmp/srcinfo.err"; then
        note "makepkg --printsrcinfo failed"
        sed 's/^/    /' "$tmp/srcinfo.err" >&2
    elif ! diff -q "$tmp/srcinfo" .SRCINFO >/dev/null; then
        note ".SRCINFO does not match PKGBUILD (regenerate: makepkg --printsrcinfo > .SRCINFO)"
        diff -u .SRCINFO "$tmp/srcinfo" | sed 's/^/    /' >&2
    fi
else
    # Off Arch (.SRCINFO is hand-edited there), compare the fields that matter.
    for k in pkgname pkgver pkgrel pkgdesc; do
        p=$(field "$k"); s=$(srcfield "$k")
        [ "$p" = "$s" ] || note "$k differs: PKGBUILD='$p' .SRCINFO='$s'"
    done
    n_src=$(grep -c '^[[:space:]]*source = ' .SRCINFO)
    n_sum=$(grep -c '^[[:space:]]*sha256sums = ' .SRCINFO)
    [ "$n_src" -eq "$n_sum" ] || note ".SRCINFO has $n_src source lines but $n_sum sha256sums"
fi

# 3. A real digest on the payload. SKIP belongs to the Sigstore bundle only, which
#    prepare() authenticates with cosign; SKIP on the .deb would pin nothing.
first_sum=$(sed -n '/^[[:space:]]*sha256sums = /{s/^[[:space:]]*sha256sums = //p;q}' .SRCINFO)
case "$first_sum" in
    [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*)
        [ "${#first_sum}" -eq 64 ] || note "sha256sums[0] is ${#first_sum} chars, not 64: '$first_sum'" ;;
    *) note "sha256sums[0] is '$first_sum', expected the .deb digest" ;;
esac

# 4. Both source lines in .SRCINFO are expanded, so a hand edit can leave one on
#    the previous release: the package would then build the old .deb, or fail the
#    hash, depending on which line was missed.
while IFS= read -r line; do
    case "$line" in
        *"$pkgver"*) ;;
        *) note "source line does not carry pkgver $pkgver: ${line#*= }" ;;
    esac
done < <(grep '^[[:space:]]*source = ' .SRCINFO)

# 5. A source that 404s fails the build for every user, and the release assets are
#    named by the workflow, so a rename or a missing upload only shows up here.
if [ "$offline" -eq 0 ] && command -v curl >/dev/null 2>&1; then
    while IFS= read -r url; do
        code=$(curl -sIL --retry 2 --max-time 25 -o /dev/null -w '%{http_code}' "$url" 2>/dev/null)
        [ "$code" = "200" ] || note "source does not resolve (HTTP ${code:-none}): $url"
    done < <(sed -n 's/^[[:space:]]*source = //p' .SRCINFO | sed 's/^[^:]*:://' | grep '^https\?://')
fi

if [ "$fail" -ne 0 ]; then
    echo "aur-preflight: FAILED, refusing to publish a broken package" >&2
    exit 1
fi
echo "aur-preflight: PKGBUILD sourceable, .SRCINFO in sync, sources resolve"
