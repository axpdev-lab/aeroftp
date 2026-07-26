#!/usr/bin/env bash
#
# Gate G1 - snap ABI floor check.
#
# A snap never ships its own glibc: the C library comes from the `base:` snap
# (core22 -> Ubuntu 22.04 -> glibc 2.35). If any ELF inside the snap was
# compiled against a newer glibc, the dynamic loader refuses it at exec time
# with `version 'GLIBC_2.x' not found` and the app cannot start at all.
#
# That is exactly what shipped between 2026-05-06 (PR #172 moved the snap build
# to an ubuntu-24.04 runner while `base:` stayed core22) and 2026-07-25: about
# thirty releases whose payload demanded GLIBC_2.38/2.39 from a 2.35 runtime.
# Nothing caught it because CI only asserted that the .snap file existed and
# was larger than 10 MB. This script is the check that would have failed PR #172
# on the day it was opened.
#
# The ceiling is DERIVED from the base snap itself, never hardcoded, so bumping
# `base:` in snapcraft.yaml automatically moves the bar with it.
#
# Usage:
#   scripts/snap-abi-check.sh <snap-file> [snapcraft-yaml]
#   scripts/snap-abi-check.sh <snap-file> --ceiling 2.39   # self-test override
#
# Exit codes: 0 = every ELF fits the base, 1 = at least one is too new,
#             2 = usage / environment error.

set -euo pipefail

SNAP_FILE=""
SNAPCRAFT_YAML="snap/snapcraft.yaml"
CEILING_OVERRIDE=""

while [ $# -gt 0 ]; do
  case "$1" in
    --ceiling)
      CEILING_OVERRIDE="${2:-}"
      shift 2
      ;;
    -h|--help)
      sed -n '2,30p' "$0"
      exit 0
      ;;
    *)
      if [ -z "$SNAP_FILE" ]; then
        SNAP_FILE="$1"
      else
        SNAPCRAFT_YAML="$1"
      fi
      shift
      ;;
  esac
done

if [ -z "$SNAP_FILE" ] || [ ! -f "$SNAP_FILE" ]; then
  echo "usage: $0 <snap-file> [snapcraft-yaml] [--ceiling X.YZ]" >&2
  exit 2
fi

for tool in unsquashfs objdump file strings; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "::error::snap-abi-check needs '$tool' (apt: squashfs-tools binutils file)" >&2
    exit 2
  }
done

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Highest GLIBC_x.y token in a stream of `GLIBC_2.35`-style strings, or empty.
max_glibc() { grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -uV | tail -1; }

# ---------------------------------------------------------------------------
# 1. The ceiling: the newest glibc version the base snap's libc.so.6 defines.
# ---------------------------------------------------------------------------
if [ -n "$CEILING_OVERRIDE" ]; then
  CEILING="GLIBC_${CEILING_OVERRIDE#GLIBC_}"
  BASE="(override)"
else
  if [ ! -f "$SNAPCRAFT_YAML" ]; then
    echo "::error::cannot read $SNAPCRAFT_YAML to resolve the snap base" >&2
    exit 2
  fi
  BASE="$(awk '/^base:[[:space:]]/ {print $2; exit}' "$SNAPCRAFT_YAML")"
  if [ -z "$BASE" ]; then
    echo "::error::no 'base:' key in $SNAPCRAFT_YAML" >&2
    exit 2
  fi

  # Prefer an already-installed base (dev machines); otherwise pull it from the
  # store. `snap download` needs no root and no login.
  # Two shapes to handle: from glibc 2.34 on, libc.so.6 IS the ELF; before that
  # it is a symlink to libc-<version>.so. `-type f` therefore matters, and the
  # pattern must stay tight enough not to match libcrypt.so.1 and friends.
  find_libc() { find -L "$1" -type f \( -name 'libc.so.6' -o -name 'libc-*.so' \) -print -quit 2>/dev/null || true; }

  # `find -L`: /snap/<base>/current is a symlink, and an unfollowed symlink
  # argument is never descended into.
  LIBC=""
  if [ -e "/snap/$BASE/current" ]; then
    LIBC="$(find_libc "/snap/$BASE/current")"
  fi
  if [ -z "$LIBC" ]; then
    echo "Downloading base snap '$BASE' to derive its glibc ceiling..."
    ( cd "$TMP" && snap download "$BASE" >/dev/null )
    BASE_SNAP="$(find "$TMP" -maxdepth 1 -name "${BASE}_*.snap" -print -quit)"
    [ -n "$BASE_SNAP" ] || { echo "::error::could not download base snap '$BASE'" >&2; exit 2; }
    # Named paths, not a full extraction: a base snap carries device nodes
    # (/dev/null, /dev/random, ...) and file capabilities, and unsquashfs
    # cannot recreate those as a normal user - it prints
    # "could not create character device ... because you're not superuser"
    # and exits non-zero. On a CI runner (no base snap installed) that turned
    # the whole gate into an environment failure. unsquashfs globs do not
    # cross '/', so the paths are spelled out per architecture.
    for triplet in x86_64-linux-gnu aarch64-linux-gnu; do
      unsquashfs -q -n -f -no-xattrs -d "$TMP/base" "$BASE_SNAP" \
        "usr/lib/$triplet/libc.so.6" "usr/lib/$triplet/libc-*.so" \
        "lib/$triplet/libc.so.6" "lib/$triplet/libc-*.so" >/dev/null 2>&1 || true
    done
    LIBC="$(find_libc "$TMP/base")"
    if [ -z "$LIBC" ]; then
      # Unknown layout: fall back to a whole-snap extraction and tolerate the
      # unavoidable device-node failures, since only libc matters here.
      unsquashfs -q -n -f -no-xattrs -d "$TMP/base-full" "$BASE_SNAP" >/dev/null 2>&1 || true
      LIBC="$(find_libc "$TMP/base-full")"
    fi
  fi
  [ -n "$LIBC" ] || { echo "::error::no libc.so.6 found in base snap '$BASE'" >&2; exit 2; }

  CEILING="$(strings -a "$LIBC" | grep -E '^GLIBC_[0-9]+\.[0-9]+$' | max_glibc)"
  [ -n "$CEILING" ] || { echo "::error::could not read glibc versions from $LIBC" >&2; exit 2; }
fi

echo "Snap:     $SNAP_FILE"
echo "Base:     $BASE"
echo "Ceiling:  $CEILING"

# ---------------------------------------------------------------------------
# 2. Every ELF in the snap - payload binaries AND staged libraries.
# ---------------------------------------------------------------------------
# -no-xattrs: an app snap can carry file capabilities that a non-root
# unsquashfs refuses to restore, and that must not read as "the snap is
# broken". A genuine extraction failure still stops the gate: a partial tree
# would silently narrow the scan.
unsquashfs -q -n -f -no-xattrs -d "$TMP/app" "$SNAP_FILE" >/dev/null

mapfile -t ELVES < <(
  find "$TMP/app" -type f -print0 \
    | xargs -0 -r file -h --mime-type -F $'\t' -- \
    | awk -F'\t' '$2 ~ /x-(executable|pie-executable|sharedlib)/ {print $1}'
)

if [ "${#ELVES[@]}" -eq 0 ]; then
  echo "::error::no ELF files found inside $SNAP_FILE - is it a valid snap?" >&2
  exit 2
fi

FAILED=0
for elf in "${ELVES[@]}"; do
  required="$(objdump -T "$elf" 2>/dev/null | max_glibc || true)"
  [ -n "$required" ] || continue
  # sort -V puts the newer version last; if that is not the ceiling, the ELF wins
  # and therefore demands more than the base can provide.
  newest="$(printf '%s\n%s\n' "$required" "$CEILING" | sort -V | tail -1)"
  if [ "$newest" != "$CEILING" ]; then
    rel="${elf#"$TMP/app/"}"
    echo "::error::$rel requires $required but base $BASE provides at most $CEILING"
    # Name the symbols that pushed it over the line - without them the failure
    # is a mystery, with them the cause (C23 stdio, pidfd spawn, ...) is obvious.
    # Kept in a `set +e` subshell: this is diagnostics, and an empty grep here
    # must never abort the scan of the remaining files.
    ( set +e +o pipefail
      objdump -T "$elf" 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -uV | while read -r ver; do
        [ -n "$ver" ] || continue
        [ "$(printf '%s\n%s\n' "$ver" "$CEILING" | sort -V | tail -1)" != "$CEILING" ] || continue
        # objdump prints required versions as "(GLIBC_2.39) symbol" and defined
        # ones as "GLIBC_2.39  symbol"; match both, with the dots escaped.
        ver_re="${ver//./\\.}"
        syms="$(objdump -T "$elf" 2>/dev/null | grep -E "\(${ver_re}\)|[[:space:]]${ver_re}[[:space:]]" \
                | awk '{print $NF}' | sort -u | head -8 | paste -sd', ' -)"
        echo "    $ver: ${syms:-<unknown>}"
      done
    ) || true
    FAILED=$((FAILED + 1))
  fi
done

echo "Scanned:  ${#ELVES[@]} ELF files"

if [ "$FAILED" -gt 0 ]; then
  echo
  echo "::error::$FAILED file(s) exceed the glibc floor of base '$BASE'."
  echo "The snap would fail at exec with \"version 'GLIBC_x.y' not found\"."
  echo "Cause is almost always a payload compiled on a newer Ubuntu than the base:"
  echo "build the binaries on the release that matches '$BASE' (core22 -> ubuntu-22.04)."
  exit 1
fi

echo "OK: every ELF fits within $CEILING."
