#!/usr/bin/env bash
#
# Is the `[patch.crates-io]` pin on n0-mainline still needed?
#
# Background: docs/security-evidence/RUSSH-DEPENDABOT-ADVISORIES-2026-07.md
#
# russh 0.62.4 closes three GHSA advisories and requires `ed25519-dalek = "^3"`,
# while n0-mainline 0.5.0 - reached through iroh-mainline-address-lookup and
# aeroftp-peer-l0 - pins `ed25519-dalek = "=3.0.0-rc.0"`. A caret requirement
# never matches a pre-release and cargo puts rc.0 and 3.0.0 in one compatibility
# class, so the two are disjoint and the graph is unsatisfiable. The bump landed
# anyway (PR #489) behind a `[patch.crates-io]` on axpdev-lab/n0-mainline, which
# is the v0.5.0 release commit plus the single line that relaxes that pin.
#
# That patch carries an exit condition - delete it once upstream admits 3.0.0
# final - and a written exit condition nobody watches stays in the tree for
# years. This script is the watcher.
#
# It replaces check-russh-unblock.sh, which asked "does russh 0.62.4 resolve?".
# That question was answered by #489: it does, *because of the patch*, so from
# the moment the bump landed the old script exited 0 unconditionally and told
# the reader to perform steps that were already done. Verified on 2026-07-28
# before removing it. A check that cannot fail is not a check.
#
# What is tested here is the question that is actually still open, and it is
# tested by doing the thing rather than by asking upstream about it: strip the
# patch section in a scratch copy of the manifest, re-resolve from scratch, and
# see whether cargo can satisfy the graph without it. That answers correctly no
# matter *why* it became possible - an upstream release, a change in the
# dependency chain, or a russh bump - and it needs no knowledge of GitHub, of
# n0-mainline's release cadence, or of any API.
#
# Exit codes:
#   0 - the patch is no longer needed (or is already gone): delete the section
#       from src-tauri/Cargo.toml, then delete this script and its workflow
#   1 - still needed, same ed25519-dalek conflict (expected, no action)
#   2 - resolution failed for some other reason: read the output before
#       assuming it is the same conflict
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR/src-tauri"

# Both halves matter. `git diff` compares the worktree with the INDEX, so a
# staged edit to either file slips past it, and the restore below is
# `git checkout HEAD --`, which would then throw that staged work away. Checking
# --cached as well means the script refuses instead of destroying it.
if ! git diff --quiet -- Cargo.toml Cargo.lock ||
   ! git diff --cached --quiet -- Cargo.toml Cargo.lock; then
  echo "refusing to run: src-tauri/Cargo.toml or Cargo.lock has uncommitted or staged changes" >&2
  echo "this check rewrites both and restores them from HEAD; commit or stash first" >&2
  exit 2
fi

if ! grep -q '^\[patch\.crates-io\]' Cargo.toml; then
  echo "NOT NEEDED - there is no [patch.crates-io] section in src-tauri/Cargo.toml."
  echo "Nothing left to watch: delete .github/scripts/check-n0-mainline-patch.sh"
  echo "and .github/workflows/n0-mainline-patch-watch.yml."
  exit 0
fi

pinned_rev="$(sed -n 's/.*n0-mainline = .*rev = "\([0-9a-f]*\)".*/\1/p' Cargo.toml | head -n 1)"
echo "patched n0-mainline rev: ${pinned_rev:-<unreadable>}"
echo "testing:                 resolution with the patch section removed"
echo

# Restore unconditionally: this rewrites both files in place, and a failed
# resolution must not leave the tree carrying a manifest nobody asked for.
restore() {
  # From HEAD, not from the index: `git checkout -- <path>` restores the STAGED
  # content, so on a tree with staged manifest edits it would silently reinstate
  # those instead of the committed state. The guard above already refuses that
  # case; this makes the restore correct on its own terms rather than relying on
  # the guard never being weakened.
  git -C "$ROOT_DIR" checkout HEAD -- src-tauri/Cargo.toml src-tauri/Cargo.lock
}
trap restore EXIT

# Drop [patch.crates-io] and everything up to the next section header or EOF.
awk '
  /^\[patch\.crates-io\]/ { skipping = 1; next }
  skipping && /^\[/       { skipping = 0 }
  !skipping               { print }
' Cargo.toml > Cargo.toml.scratch
mv Cargo.toml.scratch Cargo.toml

if grep -q '^\[patch\.crates-io\]' Cargo.toml; then
  echo "could not strip the patch section from the scratch manifest" >&2
  exit 2
fi

# generate-lockfile re-resolves the whole graph from scratch rather than reusing
# the committed lock, which still points at the patched git source.
output=""
status=0
output="$(cargo generate-lockfile 2>&1)" || status=$?
echo "$output"
echo

if [ "$status" -eq 0 ]; then
  # Report what it settled on. Resolving without the patch is the answer to the
  # question asked, but a *downgrade* of n0-mainline resolves it too, and that
  # is a judgement call rather than a green light, so both versions are printed
  # instead of being hidden behind the exit code.
  n0="$(sed -n '/^name = "n0-mainline"$/{n;s/^version = "\(.*\)"/\1/p;}' Cargo.lock | head -n 1)"
  dalek="$(sed -n '/^name = "ed25519-dalek"$/{n;s/^version = "\(.*\)"/\1/p;}' Cargo.lock | head -n 1)"
  cat <<EOF
NOT NEEDED - the graph resolves with no [patch.crates-io] section.

  n0-mainline:   ${n0:-<not in graph>}
  ed25519-dalek: ${dalek:-<not in graph>}

Check that n0-mainline above is not OLDER than the patched v0.5.0: a downgrade
also makes the conflict disappear and is not the outcome this is waiting for.

Next steps:
  1. delete the [patch.crates-io] section and its comment block from
     src-tauri/Cargo.toml
  2. cargo update -p n0-mainline, then run the pre-push gate and the
     peer/AeroShare live lane (this moves the crypto crate under the iroh P2P
     stack as well as under russh)
  3. regenerate cargo-sources.json
  4. delete this script and .github/workflows/n0-mainline-patch-watch.yml, and
     drop the follow-up section from
     docs/security-evidence/RUSSH-DEPENDABOT-ADVISORIES-2026-07.md
EOF
  exit 0
fi

# Match the resolver-conflict SIGNATURE, not the crate name. Any failure that
# merely mentions ed25519-dalek - a registry timeout, a network blip, a yanked
# release - would otherwise be classified as "still blocked" and exit 1, which
# this workflow treats as the quiet normal state. That is precisely the
# confusion the exit-2 path exists to prevent: a watcher that cannot tell "still
# blocked" from "I no longer work" is worse than none.
#
# Three markers together, and no version literals, so the check survives
# upstream moving to another release candidate: cargo only prints
# "previously selected package" when it really failed to reconcile two
# requirements, which a transport error never does.
if grep -q 'failed to select a version for' <<<"$output" &&
   grep -q 'ed25519-dalek' <<<"$output" &&
   grep -q 'previously selected package' <<<"$output"; then
  echo "STILL NEEDED - same ed25519-dalek resolver conflict, nothing to do."
  exit 1
fi

echo "FAILED FOR ANOTHER REASON - the output above is not the ed25519-dalek resolver conflict." >&2
echo "Do not read this as 'still blocked': it is either a transport/registry failure or a" >&2
echo "conflict of a different shape, and both need a human to look at the output." >&2
exit 2
