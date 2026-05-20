#!/usr/bin/env bash
# Shared helpers for the GTC parity harness.
# Sourced by parity_harness.sh; not directly executable.

set -Eeuo pipefail

# ---------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------

if [[ -t 2 && -z "${NO_COLOR:-}" ]]; then
    C_RESET=$'\033[0m'
    C_DIM=$'\033[2m'
    C_RED=$'\033[31m'
    C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m'
    C_CYAN=$'\033[36m'
else
    C_RESET=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_CYAN=""
fi

ts() { date -u +%H:%M:%S; }

log_info()  { printf '%s[%s] %s%s\n' "$C_CYAN"  "$(ts)" "$*" "$C_RESET" >&2; }
log_ok()    { printf '%s[%s] %s%s\n' "$C_GREEN" "$(ts)" "$*" "$C_RESET" >&2; }
log_warn()  { printf '%s[%s] %s%s\n' "$C_YELLOW" "$(ts)" "$*" "$C_RESET" >&2; }
log_err()   { printf '%s[%s] %s%s\n' "$C_RED"   "$(ts)" "$*" "$C_RESET" >&2; }
log_dim()   { printf '%s[%s] %s%s\n' "$C_DIM"   "$(ts)" "$*" "$C_RESET" >&2; }

# ---------------------------------------------------------------------
# Tooling
# ---------------------------------------------------------------------

require_cli() {
    if ! command -v aeroftp-cli >/dev/null 2>&1; then
        log_err "aeroftp-cli not found on PATH"
        return 1
    fi
    local v
    v=$(aeroftp-cli --version 2>/dev/null | head -n1 || true)
    log_info "aeroftp-cli: $v"
}

# Map our short tag to vault profile id. Keep in lockstep with the
# parity oracle spec section 3.
profile_id_for() {
    case "$1" in
        sftp)   echo "srv_1778600830336_6pvrx7450" ;;
        ftp)    echo "srv_axpbuntu_ftp_plain"      ;;
        ftps)   echo "srv_axpbuntu_ftps_explicit"  ;;
        s3)     echo "srv_axpbuntu_s3_minio"       ;;
        webdav) echo "srv_axpbuntu_webdav_vanilla" ;;
        *)      return 2 ;;
    esac
}

# Remote prefix where the harness puts its corpus. Lab-side cleanup
# is best-effort: don't depend on it for correctness.
#
# S3 note: the bucket is part of the connection (profile), so the
# remote path is just a KEY relative to the bucket root. Including
# the bucket name in the path would produce a doubled prefix like
# `aeroftp-test/aeroftp-test/...` and break round-trip verification.
remote_root_for() {
    case "$1" in
        sftp)   echo "/home/axpdev/_gtc_harness" ;;
        ftp|ftps) echo "/_gtc_harness" ;;
        s3)     echo "/_gtc_harness" ;;
        webdav) echo "/_gtc_harness" ;;
        *)      return 2 ;;
    esac
}

# ---------------------------------------------------------------------
# Corpus generation (reproducible per run id)
# ---------------------------------------------------------------------

gen_corpus() {
    local source_dir="$1"
    local run_id="$2"
    mkdir -p "$source_dir/small" "$source_dir/medium" "$source_dir/large"

    # The corpus is unique per run id (the run id is the UTC timestamp
    # of the harness invocation), so we read from /dev/urandom directly
    # and record the sha256 set as the byte-identity reference. We do
    # not need deterministic-from-seed content because we never reuse
    # a corpus across runs - we record the sha256 set BEFORE upload
    # and compare every roundtrip against it.

    local i
    for ((i=0; i<16; i++)); do
        local f="$source_dir/small/file_$(printf '%02d' "$i").bin"
        dd if=/dev/urandom of="$f" bs=1024 count=1 status=none
    done
    for ((i=0; i<4; i++)); do
        local f="$source_dir/medium/file_$(printf '%02d' "$i").bin"
        dd if=/dev/urandom of="$f" bs=$((512*1024)) count=1 status=none
    done
    dd if=/dev/urandom of="$source_dir/large/64MiB.bin" \
        bs=$((1024*1024)) count=64 status=none

    (cd "$source_dir" && find . -type f -print0 \
        | xargs -0 sha256sum) | sort -k2 >"$source_dir/../source.sha256"

    log_info "corpus generated under $source_dir"
    log_dim  "$(wc -c <"$source_dir/large/64MiB.bin") bytes in large/64MiB.bin"
}

# ---------------------------------------------------------------------
# Timing helpers (monotonic, sub-second)
# ---------------------------------------------------------------------

now_ms() {
    # GNU date supports %N (nanoseconds); use that and trim
    local ns
    ns=$(date +%s%N)
    echo $((ns / 1000000))
}

# ---------------------------------------------------------------------
# CLI wrappers (return exit code in $RC, wall-clock ms in $MS)
# ---------------------------------------------------------------------

cli_run() {
    # Usage: cli_run <log_file> <args...>
    local logf="$1"; shift
    local t0 t1
    t0=$(now_ms)
    set +e
    aeroftp-cli "$@" >"$logf" 2>&1
    RC=$?
    set -e
    t1=$(now_ms)
    MS=$((t1 - t0))
    return 0
}

# ---------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------

cleanup_remote() {
    local proto="$1"
    local profile_id remote_root
    profile_id=$(profile_id_for "$proto")
    remote_root=$(remote_root_for "$proto")
    log_dim "cleanup remote $proto:$remote_root (best-effort)"
    set +e
    aeroftp-cli rm -P "$profile_id" --recursive --force "$remote_root" >/dev/null 2>&1
    set -e
}
