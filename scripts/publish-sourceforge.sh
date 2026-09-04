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
# Every aeroftp-cli call in this script runs unattended, so strict mode is set
# once for all of them rather than per call: any flag that would relax a safety
# check becomes a hard error (exit 5) instead of silently proceeding, on the
# listing and delete calls of the prune as much as on the upload.
export AEROFTP_STRICT=1

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
need gh ; need jq ; need curl
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

  # An existing folder is not a failure here: a re-run after an interrupted
  # upload is the normal way this script is used a second time. SourceForge
  # answers a duplicate mkdir with a bare "Failure", which under `set -e` took
  # the whole publish down and made a COMPLETED upload look like a failed one:
  # on v4.1.9 all 17 files were already on the mirror when this line aborted.
  #
  # The message cannot be the discriminator, because "Failure" is all the
  # server says for every cause. What separates them is whether the directory
  # IS THERE afterwards: an already-existing folder lists, while a permission,
  # authentication, path or transport failure does not. So a failed mkdir is
  # tolerated only when the listing that follows it succeeds, and any other
  # cause still stops the publish before a single file is uploaded.
  if ! "$CLI" --profile "$PROFILE_ID" mkdir "$SF_ROOT/$TAG"; then
    if "$CLI" --profile "$PROFILE_ID" ls "$SF_ROOT/$TAG" >/dev/null 2>&1; then
      echo "mkdir reported a failure but $SF_ROOT/$TAG exists: continuing" >&2
    else
      echo "mkdir failed and $SF_ROOT/$TAG is not there: aborting before upload" >&2
      exit 11
    fi
  fi
  while IFS= read -r f; do
    "$CLI" --profile "$PROFILE_ID" put --partial "$f" "$SF_ROOT/$TAG/"
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
      # `entries` must be an ARRAY, not merely present: `null` or an object would
      # make every later `.entries[]` query answer "not there", and a delete loop
      # reading "not there" as "gone" would count files it never verified.
      if out="$("$CLI" --profile "$PROFILE_ID" ls "$path" --json --no-banner "$@" 2>/dev/null)" \
        && jq -e '.entries | type == "array"' >/dev/null 2>&1 <<<"$out"; then
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

  # Delete one file and let the LISTING decide whether it worked, not the exit code.
  # On the v4.1.9 prune `rm` returned exit 10 ("server or parse error") for a file that
  # was in fact gone: SourceForge had performed the delete and then answered as if it had
  # not. Under `set -e` that one false failure would abort the prune halfway and report a
  # failure for work that succeeded, and whoever re-ran it would re-delete files that were
  # already gone. It is the mirror image of the empty-listing trap above: there a failed
  # read looked like a clean folder, here a successful delete looks like a failed one.
  # So the exit status is a hint and the folder listing is the verdict: a nonzero `rm`
  # is followed by a re-list, the file being absent counts as deleted, the file being
  # present stops the script loudly. In a dry run `run` only prints, so nothing is listed.
  sfrm() {
    local folder="$1" name="$2"
    local path="$SF_ROOT/$folder/$name"
    if run "$CLI" --profile "$PROFILE_ID" rm "$path"; then
      return 0
    fi
    local after present
    after="$(sfls "$SF_ROOT/$folder" --files-only)" || exit 1
    # Three outcomes, kept apart on purpose: jq exit 0 = the file is still there,
    # exit 1 = the listing is valid and the file is absent, anything else = jq could
    # not evaluate the listing, which is a verification failure and never "gone".
    set +e
    jq -e --arg n "$name" '[.entries[] | select(.name==$n)] | length > 0' >/dev/null 2>&1 <<<"$after"
    present=$?
    set -e
    case "$present" in
      0) echo "delete failed and the file is still there: $path" >&2; exit 1 ;;
      1) echo "rm exited nonzero but $path is gone on re-listing: counted as deleted" >&2; return 0 ;;
      *) echo "delete failed and the re-listing of $SF_ROOT/$folder could not be evaluated (jq exit $present): stopping" >&2; exit 1 ;;
    esac
  }

  PRUNED=0
  for folder in "${FOLDERS[@]}"; do
    printf '%s\n' "${KEEP[@]}" | grep -qx "$folder" && continue
    FOLDER_JSON="$(sfls "$SF_ROOT/$folder" --files-only)"
    mapfile -t NAMES < <(jq -r '.entries[].name' <<<"$FOLDER_JSON")
    for name in "${NAMES[@]}"; do
      case "$name" in
        README.md|*.sigstore.json|'') continue ;;
      esac
      sfrm "$folder" "$name"
      PRUNED=$((PRUNED + 1))
    done
  done
  echo "prune: $PRUNED file(s) $([ "$FORCE" -eq 1 ] && echo deleted || echo 'would be deleted')"
fi

echo
if [ "$FORCE" -eq 1 ]; then
  # The default download button does NOT need setting by hand: that instruction stood
  # here from v4.1.3 to v4.1.9 and was false every time, SourceForge picks the newest
  # folder on its own. Verify it instead, from the same endpoint the download button
  # reads, and say so if it has not caught up yet (it can lag the upload by minutes).
  SF_PROJECT="$(basename "$SF_ROOT")"
  BEST_URL="https://sourceforge.net/projects/$SF_PROJECT/best_release.json"
  echo "Done. Checking the default download button ($BEST_URL) ..."
  # Parse and validate BEFORE looping: a jq failure inside a process substitution
  # is invisible, and a body without platform_releases would yield zero rows, so
  # the loop would end with nothing stale and the script would report a check it
  # never made. Rows must exist, and each is matched on the release FOLDER
  # `/v$VERSION/`, not on a substring: 4.1.9 must not accept 4.1.90.
  ROWS=""
  if BEST="$(curl -fsS --max-time 30 "$BEST_URL" 2>/dev/null)" \
     && ROWS="$(jq -r '.platform_releases | to_entries[] | [.key, .value.filename] | @tsv' <<<"$BEST" 2>/dev/null)" \
     && [ -n "$ROWS" ]; then
    STALE=0
    while IFS=$'\t' read -r platform filename; do
      case "$filename" in
        "/v$VERSION/"*) echo "  $platform: $filename" ;;
        *) echo "  $platform: $filename  (NOT $VERSION yet)"; STALE=1 ;;
      esac
    done <<<"$ROWS"
    if [ "$STALE" -eq 1 ]; then
      echo "  SourceForge has not switched every platform to $VERSION yet; re-check in a few minutes,"
      echo "  and only if it still lags set the default under Files > folder > (i) on the site."
    fi
  else
    echo "  could not read or parse $BEST_URL; check the download button on the project page."
  fi
else
  echo "Dry run. Re-run with --force to upload."
fi
