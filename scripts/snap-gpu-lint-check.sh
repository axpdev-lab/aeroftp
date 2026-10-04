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
#      blanket-ignored. A linter that cannot run (no build provider, no
#      network) leaves the gate green but says the lint half is UNVERIFIED,
#      never OK. Tested with stub tools: .github/scripts/test_snap_gpu_lint_check.py.
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
      # The whole comment header, however long it grows.
      awk 'NR == 1 { next } /^#/ { print; next } { exit }' "$0"
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

# We LIST the image, we do not unpack it.
#
# The first version of this gate unpacked with `-d` and tolerated a non-zero
# exit, on the reasoning that a snap carries device nodes and file
# capabilities so unsquashfs reports restore errors that are not our problem.
# That tolerance is unsafe for a gate whose only job is to fail on presence:
# an extraction that aborts part-way still leaves the destination directory
# behind, so the scan runs on a truncated tree and finds nothing.
#
# Measured, not argued (squashfs-tools 4.6.1): a 301-file image containing
# `dri/zzz_iris_dri.so`, corrupted mid-image, extracts 182 of 301 files
# WITHOUT the driver, exits 1, and leaves the directory in place. The old form
# printed "OK: no provider-owned GPU userspace inside the snap" and exited 0
# on a snap that does carry one. Reported by CodeRabbit on #485.
#
# Listing removes the whole class: the file names come from the directory
# table in one read, there is no restore step to fail on device nodes or
# capabilities, and there is no half-success to mistake for a clean result. On
# that same corrupt image `-l` still reports all 306 entries, the driver among
# them. A directory table too damaged to read exits non-zero, and that is a
# hard error here rather than something to work around.
LIST="$TMP/list.txt"
if ! unsquashfs -l "$SNAP_FILE" >"$LIST" 2>"$TMP/list.err"; then
  echo "::error::could not list $SNAP_FILE" >&2
  sed 's/^/    /' "$TMP/list.err" >&2
  exit 2
fi

ENTRIES="$(grep -c '' "$LIST" || true)"
# A snap that lists as (almost) nothing is a broken read, not a clean snap.
# Without this, an empty listing would sail through the loop below and be
# reported as "no provider-owned GPU userspace", which is the same false green
# in a different disguise.
if [ "$ENTRIES" -lt 2 ]; then
  echo "::error::listing $SNAP_FILE produced $ENTRIES entries; refusing to read that as a clean snap" >&2
  exit 2
fi

# Files the provider owns. Anything matching these inside the app snap is a
# duplicate of something `mesa-core22` will mount at $SNAP/graphics, which is
# precisely what `graphics-core22-cleanup` exists to strip.
#
# $SNAP/graphics itself is the mount point for the provider's content and is
# empty in the built snap, so it is excluded rather than matched.
#
# Each listing line is a whole path and nothing else, so a name containing
# spaces stays intact; that is why `-l` is used rather than the `-ll` long
# format, whose trailing path would have to be cut out of columns.
OFFENDERS=()
while IFS= read -r entry; do
  rel="${entry#squashfs-root/}"
  # The root entry itself carries no prefix to strip: skip it.
  [ "$rel" = "$entry" ] && continue
  case "$rel" in
    graphics | graphics/*) continue ;;
    # Both forms on purpose: paths are relative once the `squashfs-root/`
    # prefix is off, so `*/dri/...` alone would miss a `dri/` directory
    # sitting at the very root of the snap, which the previous absolute-path
    # `find` did match.
    */dri/*_dri.so | dri/*_dri.so)
      OFFENDERS+=("$rel")
      continue
      ;;
  esac
  case "${rel##*/}" in
    libEGL_mesa.so* | libGLX_mesa.so* | libgbm.so* | libdrm_*.so* | libdrm.so* | libvulkan_*.so*)
      OFFENDERS+=("$rel")
      ;;
  esac
done <"$LIST"

echo "Snap:    $SNAP_FILE"
echo "Listed:  $ENTRIES entries"

if [ "${#OFFENDERS[@]}" -gt 0 ]; then
  echo
  echo "::error::the snap primes ${#OFFENDERS[@]} provider-owned GPU file(s):"
  printf '    %s\n' "${OFFENDERS[@]}" | sort
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
LINT_RC=0
snapcraft lint "$SNAP_FILE" >"$LINT_OUT" 2>&1 || LINT_RC=$?
if [ "$LINT_RC" -ne 0 ]; then
  echo "note: snapcraft lint exited $LINT_RC; output follows"
fi
sed 's/^/    /' "$LINT_OUT"

# In debug and trace verbosity craft-cli puts a timestamp in front of every
# line ("2026-10-04 12:00:01.456 Lint warnings:"); the patterns below accept
# one, so a gpu: finding cannot pass for a clean lint behind it.
STAMP='([0-9]{4}-[0-9]{2}-[0-9]{2} [0-9:.]+ )?'

# Did the linter run? snapcraft prints its findings under a "Lint OK:",
# "Lint warnings:", "Lint errors:" or "Lint information:" header
# (snapcraft/linters/linters.py, `report`), and prints nothing at all when it
# finds nothing. So a zero exit, or a report header, means it ran; a non-zero
# exit with no header means it stopped before linting (on 2026-10-04 the build
# provider refused to start without network access). Reading the second case
# as "no gpu: lines, so clean" printed an OK nobody had verified.
if [ "$LINT_RC" -ne 0 ] && ! grep -qE "^${STAMP}[[:space:]]*Lint (OK|warnings|errors|information):" "$LINT_OUT"; then
  # `|| true`: with no output at all grep exits 1, and under pipefail the
  # assignment would end the script with exit 1, which here means "the snap
  # ships its own GPU userspace".
  reason="$(grep -v '^[[:space:]]*$' "$LINT_OUT" | tail -n 1 | cut -c1-200 || true)"
  reason="${reason//$'\r'/}"
  reason="${reason//%/%25}"
  echo
  echo "::warning::snapcraft lint did not run (${reason:-exit $LINT_RC, no output}); the gpu:/library: lint half of #465 criterion 5 is UNVERIFIED in this run"
  exit 0
fi

# Each finding is printed as "- <linter>: <file>: <text>". The leading dash is
# optional here so that a bare "gpu: ..." line still counts: matching only the
# bare form missed every real finding and printed OK over a gpu: warning.
if grep -qE "^${STAMP}[[:space:]]*(-[[:space:]]*)?gpu:" "$LINT_OUT"; then
  echo
  echo "::error::snapcraft lint still reports gpu: warnings, listed above."
  exit 1
fi

if grep -qE "^${STAMP}[[:space:]]*(-[[:space:]]*)?library:" "$LINT_OUT"; then
  echo
  echo "::warning::snapcraft lint reports library: warnings. Not a failure:" \
       "assess each one (dynamically loaded vs genuinely unused) instead of" \
       "adding a blanket ignore. See #465."
fi

echo "OK: snapcraft lint ran and reports no gpu: warnings."
