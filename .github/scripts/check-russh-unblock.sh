#!/usr/bin/env bash
#
# Periodic check for the three russh Dependabot advisories
# (GHSA-g9hv-x236-4qp3, GHSA-5xvq-cp9x-6p6r, GHSA-cqjc-rmpq-xprq).
#
# Full analysis: docs/security-evidence/RUSSH-DEPENDABOT-ADVISORIES-2026-07.md
#
# The fix is russh >= 0.62.4, blocked by a version conflict we cannot resolve
# locally: russh 0.62.4 wants `ed25519-dalek = "^3"` (which resolves to 3.0.0
# final, because a caret requirement never matches a pre-release) while
# n0-mainline 0.5.0, reached through iroh-mainline-address-lookup and
# aeroftp-peer-l0, pins `ed25519-dalek = "=3.0.0-rc.0"`. Cargo puts both in one
# compatibility class, so only one can serve the graph.
#
# Running `cargo update -p russh --precise 0.62.4` on its own does NOT test that
# conflict. Cargo.toml requires `^0.61.2`, so cargo rejects the precise version
# against the *requirement* and never reaches resolution:
#
#     error: failed to select a version for the requirement `russh = "^0.61.2"`
#     candidate versions found which didn't match: 0.62.4
#
# That error looks like a block but says nothing about upstream. This script
# raises the requirement in a scratch copy of the manifest first, so the
# resolution that actually matters is the one being tested, and restores the
# tree unconditionally on the way out.
#
# Exit codes:
#   0 - resolution succeeded: the upstream pin has cleared, the bump is now a
#       one-line change to src-tauri/Cargo.toml
#   1 - still blocked by the ed25519-dalek conflict (expected, no action)
#   2 - blocked by something else; read the output before assuming it is the
#       same conflict
set -euo pipefail

TARGET_VERSION="${1:-0.62.4}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR/src-tauri"

if ! git diff --quiet -- Cargo.toml Cargo.lock; then
  echo "refusing to run: src-tauri/Cargo.toml or Cargo.lock has uncommitted changes" >&2
  echo "this check rewrites both and restores them from git; commit or stash first" >&2
  exit 2
fi

restore() {
  git -C "$ROOT_DIR" checkout -- src-tauri/Cargo.toml src-tauri/Cargo.lock
}
trap restore EXIT

current="$(sed -n 's/^russh = { version = "\([^"]*\)".*/\1/p' Cargo.toml)"
if [ -z "$current" ]; then
  echo "could not read the russh requirement from src-tauri/Cargo.toml" >&2
  exit 2
fi
echo "shipped requirement: russh ^$current"
echo "testing:             russh $TARGET_VERSION"
echo

sed -i "s/^russh = { version = \"$current\"/russh = { version = \"$TARGET_VERSION\"/" Cargo.toml

output=""
status=0
output="$(cargo update -p russh --precise "$TARGET_VERSION" 2>&1)" || status=$?
echo "$output"
echo

if [ "$status" -eq 0 ]; then
  cat <<EOF
UNBLOCKED - russh $TARGET_VERSION now resolves.

Next steps:
  1. bump src-tauri/Cargo.toml russh to "$TARGET_VERSION"
  2. cargo update -p russh --precise $TARGET_VERSION
  3. run the pre-push gate and the peer/AeroShare live lane (this moves the
     crypto crate under the iroh P2P stack as well as under russh)
  4. close known issue #2 on the release tracker
EOF
  exit 0
fi

if grep -q 'ed25519-dalek' <<<"$output"; then
  echo "STILL BLOCKED - same ed25519-dalek conflict, nothing to do."
  echo "Plan B if this does not clear: fork n0-mainline to relax the =3.0.0-rc.0"
  echo "pin to ^3.0.0-rc.0 and land it via [patch.crates-io] on its own branch."
  exit 1
fi

echo "BLOCKED BY SOMETHING ELSE - the failure above is not the ed25519-dalek conflict." >&2
exit 2
