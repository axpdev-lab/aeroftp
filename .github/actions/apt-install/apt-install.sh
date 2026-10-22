#!/usr/bin/env bash
# Install apt packages behind a mirror that may be serving this runner at a
# few tens of kB/s, without hiding a real apt failure. Driven by action.yml in
# this directory, in three phases:
#
#   start               record the start of the whole action and print the
#                       cache prefix (before the archive restore)
#   install <pkg>...    bounded, resumable apt install (after the restore)
#   finish              hand the archives back to the runner user and say
#                       whether this run added anything worth saving
#
# Why it is shaped like this (measured 2026-10-22, ubuntu-26.04 'resolute',
# delta-sync-integration.yml runs 39794879996 and 39815053371):
#
#   * The mirror, not apt, is the failure. azure.archive.ubuntu.com served
#     74.2 MB of .deb at 43-170 kB/s on some runners and at 7 MB/s on others
#     in the same hour. Attempts of 300s were killed with the download a third
#     done, twice, and the step failed.
#   * A killed attempt loses almost nothing: completed .debs stay in the
#     archive dir, and apt 3.2 resumes the in-flight one from partial/
#     (`Range: bytes=N-` + `If-Range`, answered 206). So one attempt that owns
#     the whole budget does strictly more than two that split it, and later
#     attempts exist only for fast apt errors (a Hash Sum mismatch during a
#     mirror sync, a dropped connection).
#   * The cache is what makes a rerun converge. The old combined actions/cache
#     saved from a post-job step declared `post-if: success()`, so a job that
#     failed anywhere, apt included, never saved, and the lane whose apt kept
#     failing never got an entry at all. action.yml saves right after this
#     script, also after a failed install, including the partial files.
#
# kill(2): timeout runs as root because a runner-user timeout cannot deliver
# TERM/KILL to apt and dpkg running under sudo, which would leave the dpkg/apt
# locks held into the next attempt. nick-fields/retry dies with `kill EPERM`
# on the same thing (issue #124) and was dropped for that reason.
set -euo pipefail

archives="${APT_ARCHIVES:-$HOME/apt-archives}"
state="${RUNNER_TEMP:-/tmp}/apt-install.state"

# Names and sizes, partial files included: bytes that landed in partial/ are
# progress a rerun can resume, so they count as a change worth saving.
# Through sudo, here and in archive_bytes: apt hands partial/ to `_apt` with
# mode 0700, which the runner user cannot read until `finish` takes it back.
fingerprint() {
  local list=""
  if [ -d "$archives" ]; then
    list=$(cd "$archives" && sudo find . -maxdepth 2 -type f -name '*.deb' -printf '%p %s\n' 2>/dev/null | sort) || true
  fi
  printf '%s\n' "$list" | sha256sum | cut -c1-16
}

archive_bytes() {
  local bytes=""
  bytes=$(sudo du -sb "$archives" 2>/dev/null | cut -f1) || true
  echo "${bytes:-0}"
}

output() {
  echo "$1=$2" >> "${GITHUB_OUTPUT:-/dev/null}"
}

phase="${1:-}"
shift || true

case "$phase" in
start)
  prefix_name="${1:?cache prefix required}"
  echo "started=$(date +%s)" > "$state"
  # ImageOS keeps a 24.04 runner from restoring 26.04 archives: different
  # versions, so every one of them would be a miss and dead weight in the save.
  output prefix "${prefix_name}-${ImageOS:-unknown}-${RUNNER_ARCH:-unknown}-"
  ;;

install)
  budget="${APT_BUDGET_SECONDS:?APT_BUDGET_SECONDS required}"
  reserve="${APT_SAVE_RESERVE_SECONDS:-60}"
  kill_after=20
  [ "$#" -gt 0 ] || { echo "no packages given" >&2; exit 2; }
  # shellcheck disable=SC1090
  . "$state"
  # This phase starts where the archive restore ended, so the restore's cost
  # is on record for the one failure that happens before apt runs at all.
  restored=$(date +%s)
  echo "restored=${restored}" >> "$state"
  deadline=$((started + budget - reserve - kill_after))
  mkdir -p "$archives/partial"
  echo "before=$(fingerprint)" >> "$state"
  bytes0=$(archive_bytes)
  t0=$(date +%s)

  export RUNNER_UID RUNNER_GID APT_ARCHIVES="$archives"
  RUNNER_UID=$(id -u)
  RUNNER_GID=$(id -g)
  run_attempt() {
    set -euo pipefail
    # An attempt killed mid-unpack leaves dpkg half-configured, and the next
    # apt-get would refuse to run until it is told to finish.
    dpkg --configure -a || true
    # Runner-image extras: the preinstalled microsoft*, azure-cli* and
    # google-chrome* apt repos fail often enough to redden a run (403 on
    # InRelease, Hash Sum mismatch on Packages.gz), and nothing here installs
    # from any of them.
    rm -f /etc/apt/sources.list.d/microsoft* /etc/apt/sources.list.d/azure-cli* \
      /etc/apt/sources.list.d/google-chrome*
    # Timeout=30 turns a hung mirror connection into a retried request after
    # 30s of silence instead of minutes; it does not cut a slow transfer.
    local opts=(
      -o Acquire::Retries=2
      -o Acquire::http::Timeout=30
      -o Acquire::https::Timeout=30
      -o DPkg::Lock::Timeout=30
    )
    apt-get update "${opts[@]}"
    apt-get install -y --no-install-recommends "${opts[@]}" \
      -o Dir::Cache::archives="$APT_ARCHIVES/" \
      -o APT::Keep-Downloaded-Packages=true \
      "$@"
  }

  ok=0
  rc=0
  attempt=0
  max_attempts=3
  while [ "$attempt" -lt "$max_attempts" ]; do
    left=$((deadline - $(date +%s)))
    if [ "$left" -lt 60 ]; then
      echo "apt-install: ${left}s left before the deadline, not starting attempt $((attempt + 1))" >&2
      break
    fi
    attempt=$((attempt + 1))
    echo "::group::apt install attempt ${attempt}/${max_attempts} (up to ${left}s)"
    rc=0
    sudo --preserve-env=HOME,RUNNER_UID,RUNNER_GID,APT_ARCHIVES \
      timeout --signal=TERM --kill-after="${kill_after}s" "${left}s" \
      bash -euo pipefail -c "$(declare -f run_attempt); run_attempt \"\$@\"" _ "$@" \
      || rc=$?
    echo "::endgroup::"
    if [ "$rc" -eq 0 ]; then
      ok=1
      break
    fi
    echo "apt-install: attempt ${attempt} ended with exit ${rc}" >&2
    sudo pkill -9 -x apt-get || true
    sudo pkill -9 -x dpkg || true
    [ "$attempt" -ge "$max_attempts" ] || sleep 10
  done

  elapsed=$(( $(date +%s) - t0 ))
  fetched=$(( $(archive_bytes) - bytes0 ))
  [ "$fetched" -ge 0 ] || fetched=0
  rate=$(( fetched / (elapsed > 0 ? elapsed : 1) / 1024 ))
  summary="fetched $((fetched / 1048576)) MB in ${elapsed}s (~${rate} kB/s) over ${attempt} attempt(s)"

  if [ "$ok" -eq 1 ]; then
    # Drop .debs no current index offers any more (superseded versions), so
    # the shared cache holds one copy per package instead of every version
    # since the first save.
    sudo apt-get autoclean -o Dir::Cache::archives="$archives/" >/dev/null || true
    # Nothing is in flight after a success; a leftover partial would only ride
    # along in every later save.
    sudo find "$archives/partial" -type f -delete || true
    echo "apt-install: done, ${summary}"
    if [ "$elapsed" -gt $((budget / 2)) ]; then
      echo "::warning title=Slow apt mirror::apt install used ${elapsed}s of a ${budget}s budget: ${summary}"
    fi
    exit 0
  fi

  # No attempt started: the restore (or whatever ran before this phase) left
  # under 60s of the budget. Neither a slow mirror nor an apt error, and rc
  # is still 0, so it gets its own message rather than "apt exited 0".
  if [ "$attempt" -eq 0 ]; then
    echo "::error title=apt budget spent before apt started::No apt attempt ran: the archive restore took $((restored - started))s of a ${budget}s budget (${reserve}s reserved for the save, ${kill_after}s for kill-after), leaving under 60s for apt. Check the restore step's timing; the mirror was never contacted."
  # 124 is timeout's own TERM, 137 its KILL after kill_after: the budget ran
  # out with apt still working. Anything else is apt failing on its own
  # terms, which no budget would fix.
  elif [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    echo "::error title=apt mirror too slow::The apt install did not finish within its budget: ${summary}. Completed and partial archives are saved to the cache, so a rerun resumes from here instead of from zero."
  else
    echo "::error title=apt install failed::apt exited ${rc} on attempt ${attempt} (${summary}). This is an apt or package error, not a slow mirror; see the attempt groups above."
  fi
  exit 1
  ;;

finish)
  [ -d "$archives" ] || { output changed false; exit 0; }
  # apt ran as root and a killed attempt never reached a chown, so hand the
  # whole tree back or the save (running as the runner user) cannot read it.
  sudo chown -R "$(id -u):$(id -g)" "$archives"
  after=$(fingerprint)
  before=$(sed -n 's/^before=//p' "$state" 2>/dev/null || true)
  if [ -n "$before" ] && [ "$before" = "$after" ]; then
    echo "apt-install: archives unchanged since the restore, nothing to save"
    output changed false
  else
    echo "apt-install: archives changed (${before:-none} -> ${after}), saving"
    output changed true
  fi
  # Content-addressed: two jobs that end with the same set produce the same
  # key and the second save is a harmless "already exists".
  output fingerprint "$after"
  ;;

*)
  echo "usage: $0 start <prefix> | install <package>... | finish" >&2
  exit 2
  ;;
esac
