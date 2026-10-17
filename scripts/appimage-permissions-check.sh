#!/usr/bin/env bash
#
# The repackaged AppImage must keep Fix 4 of the "Fix AppImage" step in
# .github/workflows/build.yml: no file executable by its owner only, and no
# folder other than 755. linuxdeploy leaves AppRun.wrapped 0750, and a mount
# that keeps the stored root ownership (firejail, the AppImageHub test) then
# refuses it to every user but root.
#
# This used to be an awk one-liner over `unsquashfs -ll` inside that step, and
# it could only fail on what it recognised. If the permission column moved,
# awk matched nothing and the step passed; if unsquashfs failed, the step
# passed too, because a step without `shell:` runs as `bash -e` with no
# pipefail, so the pipeline's status was awk's. Here every line of the
# listing has to be read, and the listing has to add up to an AppImage.
#
# Usage:
#   scripts/appimage-permissions-check.sh <AppImage>
#
# Exit codes: 0 = every entry is runnable or readable by every user,
#             1 = an entry is not, 2 = usage error or a listing this check
#             cannot read. Tested with stub tools:
#             .github/scripts/test_appimage_permissions_check.py.

set -euo pipefail

APPIMAGE="${1:-}"
if [ -z "$APPIMAGE" ] || [ ! -f "$APPIMAGE" ]; then
  echo "::error::usage: $0 <AppImage>" >&2
  exit 2
fi
if ! command -v unsquashfs >/dev/null 2>&1; then
  echo "::error::unsquashfs is required to list the AppImage" >&2
  exit 2
fi

# The squashfs image starts where the runtime ELF ends; the runtime reports it.
OFFSET="$("$APPIMAGE" --appimage-offset)" || {
  echo "::error::$APPIMAGE --appimage-offset failed" >&2
  exit 2
}
if ! [[ $OFFSET =~ ^[1-9][0-9]*$ ]]; then
  echo "::error::$APPIMAGE --appimage-offset printed '$OFFSET', not an offset" >&2
  exit 2
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
LIST="$TMP/list.txt"
if ! unsquashfs -ll -o "$OFFSET" "$APPIMAGE" >"$LIST" 2>"$TMP/list.err"; then
  echo "::error::could not list $APPIMAGE at offset $OFFSET" >&2
  sed 's/^/    /' "$TMP/list.err" >&2
  exit 2
fi

# Every line of `unsquashfs -ll` is one entry: permissions, owner/group, size
# (`major, minor` for a device), date, time, and the path under
# `squashfs-root`, with no header (measured on 4.5, 4.6.1 and 4.7.5). The path
# is the last column and can hold spaces, so the line is matched as a whole.
ENTRY='^([-dlcbps][rwxsStT-]{9}) +[^ ]+ +([0-9]+|[0-9]+, *[0-9]+) +[^ ]+ +[^ ]+ (squashfs-root(/.*)?)$'
BAD=()
UNREAD=()
ROOTS=0
ENTRIES=0
APPRUN=0
# `|| [ -n "$line" ]`: a last line without a newline is still an entry.
while IFS= read -r line || [ -n "$line" ]; do
  if ! [[ $line =~ $ENTRY ]]; then
    UNREAD+=("$line")
    continue
  fi
  mode="${BASH_REMATCH[1]}"
  path="${BASH_REMATCH[3]}"
  case "$path" in
    squashfs-root) ROOTS=$((ROOTS + 1)) ;;
    squashfs-root/AppRun | "squashfs-root/AppRun -> "*) APPRUN=1 ;;
  esac
  ENTRIES=$((ENTRIES + 1))
  case "$mode" in
    # A regular file its owner can run must be runnable by everyone, and
    # AppRun, the entry point, must be runnable at all.
    -??x*) [ "${mode:9:1}" = x ] || BAD+=("$mode $path") ;;
    -*) [ "$path" != squashfs-root/AppRun ] || BAD+=("$mode $path") ;;
    d*) [ "$mode" = drwxr-xr-x ] || BAD+=("$mode $path") ;;
  esac
done <"$LIST"

# What was read has to add up to an AppImage: one root, an AppRun at its top,
# and no line left unread. An empty listing fails here too.
if [ "${#UNREAD[@]}" -gt 0 ] || [ "$ROOTS" -ne 1 ] || [ "$APPRUN" -ne 1 ]; then
  echo "::error::cannot read the listing of $APPIMAGE: ${#UNREAD[@]} unread line(s), root listed $ROOTS time(s), AppRun found $APPRUN time(s), $ENTRIES entries" >&2
  if [ "${#UNREAD[@]}" -gt 0 ]; then
    printf '    unread: %s\n' "${UNREAD[@]:0:5}" >&2
  fi
  exit 2
fi

if [ "${#BAD[@]}" -gt 0 ]; then
  echo "::error::AppImage entries not runnable or readable by every user:"
  printf '%s\n' "${BAD[@]}"
  exit 1
fi

echo "OK: $ENTRIES AppImage entries, every one runnable or readable by every user."
