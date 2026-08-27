#!/usr/bin/env bash
# Mechanical Linux ACL runtime-contract checks for AeroRsync (G4).
#
# Release binary with AeroRsync enabled must DT_NEEDED libacl.so.1.
# .deb must declare libacl1. .rpm must auto-require libacl.so.1 (ELF).
# Snap must contain libacl.so.1. AppImage must keep the system-library
# contract: the payload still needs libacl.so.1, but must not bundle it.
#
# --require-bundles: fail if an expected artifact class is missing.
# SNAP_FILE: optional path to a .snap to inspect.

set -euo pipefail

REQUIRE=0
SNAP_FILE="${SNAP_FILE:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --require-bundles) REQUIRE=1; shift ;;
    --snap) SNAP_FILE="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE="$ROOT/src-tauri/target/release"
BUNDLE="$RELEASE/bundle"
failed=0

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing tool: $1" >&2
    exit 2
  }
}

fail() {
  echo "::error::$1" >&2
  failed=1
}

require_or_skip() {
  local kind="$1"
  local found="$2"
  if [ -n "$found" ]; then
    return 0
  fi
  if [ "$REQUIRE" -eq 1 ]; then
    fail "expected $kind artifact is missing"
  else
    echo "no $kind artifact present (not required on this run)"
  fi
  return 1
}

check_dt_needed() {
  local bin="$1"
  need_cmd readelf
  if ! readelf -d "$bin" | grep -q 'NEEDED[[:space:]]*\[libacl\.so\.1\]'; then
    fail "$bin does not DT_NEEDED libacl.so.1"
    return
  fi
  echo "OK: $(basename "$bin") DT_NEEDED libacl.so.1"
}

BIN=""
for candidate in "$RELEASE/aeroftp" "$RELEASE/aeroftp.bin"; do
  if [ -f "$candidate" ]; then
    BIN="$candidate"
    break
  fi
done
if require_or_skip "release binary" "$BIN"; then
  check_dt_needed "$BIN"
fi

DEB=""
shopt -s nullglob
debs=("$BUNDLE"/deb/*.deb)
if [ ${#debs[@]} -gt 0 ]; then
  DEB="${debs[0]}"
fi
if require_or_skip ".deb" "$DEB"; then
  need_cmd dpkg-deb
  depends="$(dpkg-deb -f "$DEB" Depends || true)"
  if ! printf '%s\n' "$depends" | grep -Eq '(^|,)[[:space:]]*libacl1([, ]|$)'; then
    fail "$DEB Depends does not declare libacl1: $depends"
  else
    echo "OK: $(basename "$DEB") Depends includes libacl1"
  fi
fi

RPM=""
rpms=("$BUNDLE"/rpm/*.rpm)
if [ ${#rpms[@]} -gt 0 ]; then
  RPM="${rpms[0]}"
fi
if require_or_skip ".rpm" "$RPM"; then
  need_cmd rpm
  reqs="$(rpm -qpR "$RPM" || true)"
  if ! printf '%s\n' "$reqs" | grep -q 'libacl\.so\.1'; then
    fail "$RPM does not auto-require libacl.so.1: $reqs"
  else
    echo "OK: $(basename "$RPM") Requires libacl.so.1"
  fi
fi

APPIMAGE=""
appimages=("$BUNDLE"/appimage/*.AppImage)
if [ ${#appimages[@]} -gt 0 ]; then
  APPIMAGE="${appimages[0]}"
fi
if require_or_skip "AppImage" "$APPIMAGE"; then
  need_cmd unsquashfs
  need_cmd readelf
  chmod +x "$APPIMAGE" || true
  offset="$("$APPIMAGE" --appimage-offset)"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  unsquashfs -d "$tmp/root" -o "$offset" "$APPIMAGE" >/dev/null
  inner="$(find "$tmp/root" -type f \( -name aeroftp -o -name aeroftp.bin \) | head -1)"
  if [ -z "$inner" ]; then
    fail "AppImage payload binary not found"
  else
    check_dt_needed "$inner"
  fi
  if find "$tmp/root" -name 'libacl.so*' | grep -q .; then
    fail "AppImage bundles libacl; the system-library contract forbids a special-case bundle"
  else
    echo "OK: AppImage does not bundle libacl.so*"
  fi
  trap - RETURN
  rm -rf "$tmp"
fi

if [ -n "$SNAP_FILE" ]; then
  if [ ! -f "$SNAP_FILE" ]; then
    fail "SNAP_FILE is set but missing: $SNAP_FILE"
  else
    need_cmd unsquashfs
    tmp="$(mktemp -d)"
    unsquashfs -d "$tmp/snap" "$SNAP_FILE" >/dev/null
    if ! find "$tmp/snap" -name 'libacl.so.1*' | grep -q .; then
      fail "snap does not contain libacl.so.1"
    else
      echo "OK: $(basename "$SNAP_FILE") contains libacl.so.1"
    fi
    rm -rf "$tmp"
  fi
elif [ "$REQUIRE" -eq 1 ]; then
  echo "snap not supplied on this run (SNAP_FILE unset); linux bundle job does not produce it"
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi
echo "Linux ACL packaging contracts passed"
