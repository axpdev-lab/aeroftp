#!/usr/bin/env bash
# Publish a released version to the SourceForge file area, using AeroFTP's own CLI.
#
# SourceForge used to mirror every GitHub Release whole, through a GitHub webhook on the
# `release` event. That webhook was removed on 2026-07-10: 93 releases mirrored whole had
# grown the project to 44.5 GiB and earned a storage warning. Nothing publishes to
# SourceForge automatically any more, so this script is the step that does it, and it
# uploads a curated subset rather than everything.
#
# Station-agnostic on purpose: the profile is resolved BY NAME from the vault, never by a
# hardcoded id (ids differ per station), and every path is derived from the repo.
#
#   ./scripts/publish-sourceforge.sh 4.1.3            # dry run, prints what it would do
#   ./scripts/publish-sourceforge.sh 4.1.3 --force    # actually upload
#   ./scripts/publish-sourceforge.sh 4.1.3 --force --prune   # also prune old folders
#
set -euo pipefail

PROFILE_NAME="${SF_PROFILE_NAME:-SourceForge}"
REPO="${SF_GH_REPO:-axpdev-lab/aeroftp}"
SF_ROOT="${SF_ROOT:-/home/frs/project/aeroftp}"
KEEP_COMPLETE="${SF_KEEP_COMPLETE:-2}"   # newest N release folders keep their full asset set

VERSION="" ; FORCE=0 ; PRUNE=0
for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    --prune) PRUNE=1 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    -*) echo "unknown flag: $arg" >&2; exit 2 ;;
    *) VERSION="${arg#v}" ;;
  esac
done
[ -n "$VERSION" ] || { echo "usage: $0 <X.Y.Z> [--force] [--prune]" >&2; exit 2; }

# Dry run is the default. Deleting 846 files by hand once was enough: nothing here
# uploads or removes anything until you say --force.
# Print the dry-run command SHELL-QUOTED. Half the SourceForge filenames contain spaces
# ("AeroFTP v4.1.1 source code.zip"); printing them bare invites a copy-paste that deletes
# the wrong thing.
run() {
  if [ "$FORCE" -eq 1 ]; then "$@"; else printf 'DRY-RUN: %s\n' "$(printf '%q ' "$@")"; fi
}

need() { command -v "$1" >/dev/null || { echo "missing: $1" >&2; exit 1; }; }
need gh ; need jq
CLI="${AEROFTP_CLI:-$(command -v aeroftp-cli || echo /usr/bin/aeroftp-cli)}"
[ -x "$CLI" ] || { echo "missing: aeroftp-cli" >&2; exit 1; }

# Resolve the profile by name. Ids are per-vault, so hardcoding one only works on the
# station it was written on.
PROFILE_ID="$("$CLI" profiles --json | jq -r --arg n "$PROFILE_NAME" \
  '(if type=="array" then . else .profiles end)[] | select(.name==$n) | .id' | head -1)"
[ -n "$PROFILE_ID" ] || { echo "no vault profile named '$PROFILE_NAME'" >&2; exit 1; }
echo "profile: $PROFILE_NAME ($PROFILE_ID)"

TAG="v$VERSION"
gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1 || {
  echo "no published GitHub Release $TAG on $REPO" >&2; exit 1; }

# Plan from the API, never from a download: a dry run that pulls 690 MiB to tell you what
# it would have pulled is not a dry run. `assets` excludes GitHub's auto-generated source
# archives, so they never enter the picture. Drop the snap (the Snap Store serves it, and
# at 287 MB it was about 30% of a release folder) and its sigstore, which attests a file
# we deliberately do not ship here.
mapfile -t ASSETS < <(gh release view "$TAG" --repo "$REPO" --json assets \
  -q '.assets[] | select(.name | endswith(".snap") or endswith(".snap.sigstore.json") | not) | .name')
[ "${#ASSETS[@]}" -gt 0 ] || { echo "release $TAG has no assets to publish" >&2; exit 1; }
BYTES=$(gh release view "$TAG" --repo "$REPO" --json assets \
  -q '[.assets[] | select(.name | endswith(".snap") or endswith(".snap.sigstore.json") | not) | .size] | add')

echo "to upload: $((${#ASSETS[@]} + 1)) files (incl. README.md), $((BYTES/1024/1024)) MiB -> $SF_ROOT/$TAG"
printf '  %s\n' "${ASSETS[@]}" | sort
echo "  README.md (from the release notes)"

if [ "$FORCE" -eq 0 ]; then
  echo
  echo "Dry run: nothing downloaded, nothing uploaded. Re-run with --force."
  [ "$PRUNE" -eq 0 ] && exit 0
fi

if [ "$FORCE" -eq 1 ]; then
  STAGE="$(mktemp -d)" ; trap 'rm -rf "$STAGE"' EXIT
  echo "staging: $STAGE"
  # Release notes become the folder README that SourceForge renders under the file list.
  # The webhook used to do this for us.
  gh release view "$TAG" --repo "$REPO" --json body -q .body > "$STAGE/README.md"
  # A footer rendered only on the SourceForge file page, recording that the
  # release was uploaded through the application's own SFTP integration.
  cat >> "$STAGE/README.md" <<'SFNOTE'

---

_This release was published to SourceForge through AeroFTP's own SFTP integration. The artifacts on this page were uploaded with `aeroftp-cli`, the same secure file-transfer engine that AeroFTP provides to its users. See the [SourceForge provider guide](https://docs.aeroftp.app/providers/sourceforge.html) to connect AeroFTP to SourceForge yourself._
SFNOTE
  for a in "${ASSETS[@]}"; do
    gh release download "$TAG" --repo "$REPO" --dir "$STAGE" --pattern "$a" --skip-existing
  done

  "$CLI" --profile "$PROFILE_ID" mkdir "$SF_ROOT/$TAG"
  while IFS= read -r f; do
    "$CLI" --profile "$PROFILE_ID" put "$f" "$SF_ROOT/$TAG/"
  done < <(find "$STAGE" -maxdepth 1 -type f | sort)

  echo "--- remote listing after upload ---"
  "$CLI" --profile "$PROFILE_ID" ls "$SF_ROOT/$TAG"
fi

# Retention. Every folder older than the newest KEEP_COMPLETE keeps only its README and
# its sigstore attestations: the binaries live on GitHub Releases (93 releases, verified),
# and the aggregate SourceForge download counters survive the delete (the per-file ones do
# not, and are lost either way).
if [ "$PRUNE" -eq 1 ]; then
  echo "--- prune (keeping the newest $KEEP_COMPLETE complete) ---"
  # Parse JSON, never the text listing: it carries a banner line, a trailing summary, and
  # a `/` suffix on directory names. Scraping it silently matched nothing the first time,
  # which is the worst way for a delete loop to fail: it looks like it worked.
  #
  # And fail LOUDLY on a failed listing. The CLI prints `{"status":"error",...}` and exits
  # nonzero, but inside a process substitution that exit status is lost, `jq '.entries[]'`
  # yields nothing, and an empty folder is indistinguishable from a clean one. SourceForge
  # started refusing SSH connections mid-test (it rate-limits) and the script cheerfully
  # reported "0 files would be deleted" for eleven folders it never managed to read.
  #
  # SourceForge rate-limits and times out under a burst of SFTP sessions (it refused us
  # outright after a few hundred operations), so retry a couple of times with a pause
  # before giving up. Give up loudly, never quietly.
  sfls() {
    local path="$1"; shift
    local out attempt
    for attempt in 1 2 3; do
      if out="$("$CLI" --profile "$PROFILE_ID" ls "$path" --json --no-banner "$@" 2>/dev/null)" \
        && jq -e 'has("entries")' >/dev/null 2>&1 <<<"$out"; then
        printf '%s' "$out"
        return 0
      fi
      [ "$attempt" -lt 3 ] && sleep $((attempt * 10))
    done
    echo "listing failed after 3 attempts: $path" >&2
    # Print the raw last response. Piping it through jq here would replace the real cause
    # ("Connection refused", "SFTP session Timeout") with a jq parse error.
    echo "  last response: ${out:-<no output>}" >&2
    echo "  SourceForge rate-limits bursts of SFTP sessions; wait a few minutes." >&2
    exit 1
  }

  # `mapfile < <(sfls ...)` would run sfls in a subshell, where its `exit 1` on a failed
  # listing kills only that subshell and leaves the caller with an empty array. Capture
  # into a variable first, so a failure actually stops the script.
  ROOT_JSON="$(sfls "$SF_ROOT" --dirs-only)"
  mapfile -t FOLDERS < <(jq -r '.entries[].name' <<<"$ROOT_JSON" \
    | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V)
  [ "${#FOLDERS[@]}" -gt 0 ] || { echo "no release folders found under $SF_ROOT" >&2; exit 1; }
  mapfile -t KEEP < <(printf '%s\n' "${FOLDERS[@]}" | tail -n "$KEEP_COMPLETE")
  echo "keeping complete: ${KEEP[*]}"

  PRUNED=0
  for folder in "${FOLDERS[@]}"; do
    printf '%s\n' "${KEEP[@]}" | grep -qx "$folder" && continue
    FOLDER_JSON="$(sfls "$SF_ROOT/$folder" --files-only)"
    mapfile -t NAMES < <(jq -r '.entries[].name' <<<"$FOLDER_JSON")
    for name in "${NAMES[@]}"; do
      case "$name" in
        README.md|*.sigstore.json|'') continue ;;
      esac
      run "$CLI" --profile "$PROFILE_ID" rm "$SF_ROOT/$folder/$name"
      PRUNED=$((PRUNED + 1))
    done
  done
  echo "prune: $PRUNED file(s) $([ "$FORCE" -eq 1 ] && echo deleted || echo 'would be deleted')"
fi

echo
if [ "$FORCE" -eq 1 ]; then
  echo "Done. Now set the SourceForge default download button by hand: the webhook used"
  echo "to do it, and without it the project page keeps offering the previous release."
else
  echo "Dry run. Re-run with --force to upload."
fi
