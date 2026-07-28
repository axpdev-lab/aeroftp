#!/usr/bin/env bash
#
# Gate G4 - the snap must not carry its own GPU userspace.
#
# Hardware-specific GL userspace has to come from a graphics content provider
# (`graphics-core22` / `mesa-core22`), never from the application snap. A snap
# that primes its own Mesa DRI/EGL/GLX/libdrm stack mixes loader and driver
# across the confinement boundary, which is what broke swrast while the blank
# WebKit window of #462 was being diagnosed.
#
# `snap/snapcraft.yaml` already does the right thing: the `graphics-core22`
# part runs Canonical's `graphics-core22-cleanup` at prime time. Nothing
# checked that it kept working. This is that check, and it is the last open
# acceptance criterion of #465.
#
# Two independent passes, because they fail for different reasons:
#
#   1. Content check (authoritative here, always runs). Unpacks the snap and
#      looks for provider-owned files. It needs only `unsquashfs`, so it never
#      degrades into "could not verify" on a runner.
#   2. `snapcraft lint`, when snapcraft is available. Its `gpu:` warnings are
#      the upstream wording of the same defect, and its remaining `library:`
#      warnings are printed for a human to judge one by one. Those are NOT
#      failed on: dynamic loading legitimately looks like an unused library,
#      and #465 asks for them to be assessed individually rather than
#      blanket-ignored.
#
# Usage:
#   scripts/snap-gpu-lint-check.sh <snap-file>
#   scripts/snap-gpu-lint-check.sh <snap-file> --no-snapcraft   # content only
#
# Exit codes: 0 = no provider-owned GPU userspace inside the snap,
#             1 = the snap ships its own, 2 = usage / environment error.

set -euo pipefail

SNAP_FILE=""
RUN_SNAPCRAFT=1

while [ $# -gt 0 ]; do
  case "$1" in
    --no-snapcraft)
      RUN_SNAPCRAFT=0
      shift
      ;;
    -h|--help)
      sed -n '2,33p' "$0"
      exit 0
      ;;
    *)
      SNAP_FILE="$1"
      shift
      ;;
  esac
done

if [ -z "$SNAP_FILE" ] || [ ! -f "$SNAP_FILE" ]; then
  echo "::error::usage: $0 <snap-file> [--no-snapcraft]" >&2
  exit 2
fi

if ! command -v unsquashfs >/dev/null 2>&1; then
  echo "::error::unsquashfs is required to inspect the snap" >&2
  exit 2
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Same defensive form as the ABI gate: a snap carries device nodes and file
# capabilities, so unsquashfs can report restore errors that are not our
# problem. Tolerate a non-zero exit and judge on what actually landed.
unsquashfs -q -n -f -no-xattrs -d "$TMP/snap" "$SNAP_FILE" >/dev/null 2>&1 || true
if [ ! -d "$TMP/snap" ]; then
  echo "::error::could not unpack $SNAP_FILE" >&2
  exit 2
fi

# Files the provider owns. Anything matching these inside the app snap is a
# duplicate of something `mesa-core22` will mount at $SNAP/graphics, which is
# precisely what `graphics-core22-cleanup` exists to strip.
#
# $SNAP/graphics itself is the mount point for the provider's content and is
# empty in the built snap, so it is excluded rather than matched.
mapfile -t OFFENDERS < <(
  find "$TMP/snap" \
    -path "$TMP/snap/graphics" -prune -o \
    \( \
      -path '*/dri/*_dri.so' -o \
      -name 'libEGL_mesa.so*' -o \
      -name 'libGLX_mesa.so*' -o \
      -name 'libgbm.so*' -o \
      -name 'libdrm_*.so*' -o \
      -name 'libdrm.so*' -o \
      -name 'libvulkan_*.so*' \
    \) -print 2>/dev/null | sort
)

echo "Snap:     $SNAP_FILE"
echo "Unpacked: $(find "$TMP/snap" -type f | wc -l) files"

if [ "${#OFFENDERS[@]}" -gt 0 ]; then
  echo
  echo "::error::the snap primes ${#OFFENDERS[@]} provider-owned GPU file(s):"
  for f in "${OFFENDERS[@]}"; do
    echo "    ${f#"$TMP/snap/"}"
  done
  echo
  echo "GPU userspace must come from the graphics-core22 provider, not from"
  echo "this snap. Check that the 'graphics-core22' part still runs"
  echo "graphics-core22-cleanup at prime time in snap/snapcraft.yaml, and that"
  echo "its 'after:' still lists every part that stages GTK/WebKit."
  exit 1
fi

echo "OK: no provider-owned GPU userspace inside the snap."

# ---------------------------------------------------------------------------
# Second pass, advisory except for `gpu:`.
# ---------------------------------------------------------------------------
if [ "$RUN_SNAPCRAFT" -eq 0 ]; then
  exit 0
fi

if ! command -v snapcraft >/dev/null 2>&1; then
  echo "note: snapcraft not on PATH, skipping the linter pass (content check already passed)"
  exit 0
fi

echo
echo "Running snapcraft lint ..."
LINT_OUT="$TMP/lint.txt"
# The linter is allowed to fail: it needs a build instance and may be
# unavailable on a given runner. Its absence must not turn a green content
# check into a red gate, and its presence must not hide a gpu: warning.
if ! snapcraft lint "$SNAP_FILE" >"$LINT_OUT" 2>&1; then
  echo "note: snapcraft lint exited non-zero; output follows"
fi
sed 's/^/    /' "$LINT_OUT"

if grep -qE '^\s*gpu:' "$LINT_OUT"; then
  echo
  echo "::error::snapcraft lint still reports gpu: warnings, listed above."
  exit 1
fi

if grep -qE '^\s*library:' "$LINT_OUT"; then
  echo
  echo "::warning::snapcraft lint reports library: warnings. Not a failure:" \
       "assess each one (dynamically loaded vs genuinely unused) instead of" \
       "adding a blanket ignore. See #465."
fi

echo "OK: snapcraft lint reports no gpu: warnings."
