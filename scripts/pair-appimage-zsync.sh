#!/usr/bin/env bash
# Keep the published AppImage and its .zsync from the same build.
#
# The publish job uploads with overwrite_files: false, which repairs a partial
# release by uploading only the missing assets. A re-run whose fresh build
# produced a byte-different AppImage would then publish a .zsync that does not
# match the AppImage already on the release, breaking delta updates. When the
# release already has the AppImage but not its .zsync, this regenerates the
# .zsync from the published bytes instead of shipping this run's one.
#
# Usage: scripts/pair-appimage-zsync.sh <directory with this build's AppImages>
# Reads GITHUB_REPOSITORY and GITHUB_REF_NAME; gh reads GH_TOKEN.
#
# The asset list comes from `gh api --include`, which prints the status line,
# the headers, a blank line and the JSON body. The status line tells a release
# that does not exist yet from a lookup that failed. The body must parse as
# exactly one release with this tag and an asset list: anything else refuses,
# because an empty asset list is what a fresh release looks like and pairing
# against it skips both checks below.
#
# Exit codes: 0 paired, nothing to pair, or no release yet; 1 refused.
# Tests: .github/scripts/test_pair_appimage_zsync.py
set -euo pipefail
shopt -s nullglob

DIR="${1:?usage: pair-appimage-zsync.sh <directory with the AppImages of this build>}"
: "${GITHUB_REPOSITORY:?}" "${GITHUB_REF_NAME:?}"

appimages=("$DIR"/*.AppImage)
if [ ${#appimages[@]} -eq 0 ]; then exit 0; fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if ! gh api --include "repos/$GITHUB_REPOSITORY/releases/tags/$GITHUB_REF_NAME" >"$TMP/release.txt" 2>"$TMP/release.err"; then
  if grep -Eq '^HTTP/[0-9.]+ 404([[:space:]]|$)' "$TMP/release.txt"; then
    echo "Release $GITHUB_REF_NAME does not exist yet: fresh publish, the pair comes from this build"
    exit 0
  fi
  echo "::error::Release lookup failed; refusing to assume a fresh publication" >&2
  cat "$TMP/release.err" >&2
  exit 1
fi

sed '1,/^[[:space:]]*$/d' "$TMP/release.txt" >"$TMP/release.json"
if ! jq -se --arg tag "$GITHUB_REF_NAME" \
  'length == 1 and (.[0] | .tag_name == $tag and (.assets | type) == "array" and all(.assets[]; (.name | type) == "string"))' \
  "$TMP/release.json" >/dev/null 2>&1; then
  echo "::error::cannot read release $GITHUB_REF_NAME and its assets from the gh api --include output; refusing to pair without the published asset list" >&2
  head -c 2000 "$TMP/release.txt" >&2
  exit 1
fi
jq -r '.assets[].name' "$TMP/release.json" >"$TMP/assets.txt"

for appimage in "${appimages[@]}"; do
  base=$(basename "$appimage")
  if grep -qxF "${base}.zsync" "$TMP/assets.txt" && ! grep -qxF "$base" "$TMP/assets.txt"; then
    echo "::error::Release has ${base}.zsync without its AppImage; refusing to pair it with new bytes" >&2
    exit 1
  fi
  if grep -qxF "$base" "$TMP/assets.txt" && ! grep -qxF "${base}.zsync" "$TMP/assets.txt"; then
    echo "Repair case: the release has $base but no .zsync; regenerating from the published asset"
    # Two commands, not an && list: set -e does not stop on a failure inside
    # one, so a failed update would skip the install and download anyway.
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends zsync
    mkdir -p "$TMP/published"
    gh release download "$GITHUB_REF_NAME" --repo "$GITHUB_REPOSITORY" --pattern "$base" --dir "$TMP/published" --clobber
    (cd "$TMP/published" && zsyncmake "$base")
    mv "$TMP/published/${base}.zsync" "${appimage}.zsync"
  fi
done
