#!/usr/bin/env bash
# GTC parity harness - drives the GUI<->CLI parity oracle on the
# axpbuntu lab profiles. See tests/gtc/README.md for usage and
# docs/dev/roadmap/APPENDIX-GUI-TRANSFER-CONVERGENCE/baselines/
# 2026-05-19_parity-oracle-spec.md for the contract.
#
# GTC-0 scope: CLI surfaces only (segments-1 baseline, segments-n DAG,
# parallel-n batch). GUI surfaces are wired in GTC-1+ as each Tauri
# command starts routing through ProviderTransferExecutor.

set -Eeuo pipefail

HARNESS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$HARNESS_DIR/../.." && pwd)
# shellcheck disable=SC1091
source "$HARNESS_DIR/lib/common.sh"

# ---------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------

PROTOCOLS_DEFAULT="sftp,ftp,ftps,s3,webdav"
PROTOCOLS=""
DRY_RUN=0
KEEP_CORPUS=0
BANDS_FILE=""

usage() {
    cat >&2 <<EOF
Usage: tests/gtc/parity_harness.sh [options]

Options:
  --protocols LIST    Comma-separated subset (default: $PROTOCOLS_DEFAULT)
  --dry-run           Generate corpus + plan, do not transfer
  --keep-corpus       Keep generated corpus after run
  --band PROTO:F:C    Override speedup band for one protocol (repeatable)
  -h, --help          Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --protocols) PROTOCOLS="$2"; shift 2 ;;
        --dry-run)   DRY_RUN=1;     shift ;;
        --keep-corpus) KEEP_CORPUS=1; shift ;;
        --band)
            mkdir -p "$HARNESS_DIR/reports/.tmp"
            BANDS_FILE="$HARNESS_DIR/reports/.tmp/bands.$$"
            echo "$2" >>"$BANDS_FILE"
            shift 2 ;;
        -h|--help)   usage; exit 0 ;;
        *)           log_err "unknown flag: $1"; usage; exit 2 ;;
    esac
done

PROTOCOLS="${PROTOCOLS:-$PROTOCOLS_DEFAULT}"
IFS=',' read -r -a PROTO_LIST <<<"$PROTOCOLS"

# ---------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------

require_cli

RUN_ID=$(date -u +%Y%m%dT%H%M%SZ)
RUN_DIR="$HARNESS_DIR/reports/$RUN_ID"
CORPUS_DIR="$HARNESS_DIR/corpus/$RUN_ID"
SOURCE_DIR="$CORPUS_DIR/source"
mkdir -p "$RUN_DIR" "$SOURCE_DIR"

log_info "run id: $RUN_ID"
log_info "report dir: $RUN_DIR"
log_info "protocols: ${PROTO_LIST[*]}"

# Validate profile ids exist in the vault before doing anything
VAULT_JSON=$(aeroftp-cli profiles --format json 2>/dev/null || echo '[]')
if [[ "$VAULT_JSON" == "[]" || -z "$VAULT_JSON" ]]; then
    log_err "vault returned empty profile list - check AEROFTP_MASTER_PASSWORD or unlock keyring"
    exit 5
fi

for p in "${PROTO_LIST[@]}"; do
    pid=$(profile_id_for "$p") || { log_err "unknown protocol tag: $p"; exit 2; }
    if ! printf '%s' "$VAULT_JSON" | python3 -c "
import json,sys
ids=[x.get('id') for x in json.load(sys.stdin)]
sys.exit(0 if '$pid' in ids else 1)
" 2>/dev/null; then
        log_err "profile id $pid (for $p) not in vault"
        exit 5
    fi
done

gen_corpus "$SOURCE_DIR" "$RUN_ID"

if [[ "$DRY_RUN" == "1" ]]; then
    log_ok "dry-run complete; corpus at $SOURCE_DIR"
    exit 0
fi

# ---------------------------------------------------------------------
# Cells
# ---------------------------------------------------------------------

REPORT_JSON="$RUN_DIR/report.json"
REPORT_MD="$RUN_DIR/report.md"
echo '[]' >"$RUN_DIR/cells.json"

# Append one cell row to cells.json. We pass values as argv to avoid
# shell-vs-python boolean / null literal mismatches.
emit_cell() {
    python3 "$HARNESS_DIR/lib/emit_cell.py" "$RUN_DIR/cells.json" \
        "$SURFACE" "$PROTOCOL" "$FILE_TAG" \
        "$WALL_S" "${BASELINE_S:-}" "${SPEEDUP:-}" \
        "${BAND_FLOOR:-}" "${BAND_CEIL:-}" \
        "$SHA_SRC" "$SHA_RT" "$BYTE_IDENT" \
        "$EXPECTED_KIND" "$RC_OUT" "$PASSED"
}

band_for() {
    # Returns "floor ceiling" honoring --band overrides else the spec.
    local proto="$1"
    if [[ -n "$BANDS_FILE" && -f "$BANDS_FILE" ]]; then
        local override
        override=$(grep "^${proto}:" "$BANDS_FILE" | tail -n1 || true)
        if [[ -n "$override" ]]; then
            echo "$override" | awk -F: '{print $2" "$3}'
            return
        fi
    fi
    case "$proto" in
        sftp)   echo "1.5 6.0" ;;
        ftp)    echo "1.5 5.0" ;;
        ftps)   echo "1.3 5.0" ;;
        s3)     echo "2.0 6.0" ;;
        webdav) echo "1.3 4.0" ;;
        *)      echo "1.0 99.0" ;;
    esac
}

run_one_protocol() {
    local proto="$1"
    local profile_id remote_root
    profile_id=$(profile_id_for "$proto")
    remote_root=$(remote_root_for "$proto")

    log_info "=== protocol: $proto (profile $profile_id) ==="

    # Always start with a clean remote root.
    cleanup_remote "$proto"

    # Ensure remote_root exists as a dir (idempotent, best-effort).
    set +e
    aeroftp-cli mkdir -P "$profile_id" -p "$remote_root" >"$RUN_DIR/${proto}_mkdir.log" 2>&1
    set -e

    local rt_dir="$CORPUS_DIR/roundtrip/${proto}"
    mkdir -p "$rt_dir"

    # 1) Upload the corpus (segments-1 / parallel-1 baseline).
    log_info "[$proto] uploading corpus (single-stream)"
    local logf="$RUN_DIR/${proto}_upload.log"
    cli_run "$logf" put -P "$profile_id" --parallel 1 -r "$SOURCE_DIR" "$remote_root"
    if [[ "$RC" -ne 0 ]]; then
        log_err "[$proto] upload failed (rc=$RC); see $logf"
        SURFACE="upload"; PROTOCOL="$proto"; FILE_TAG="corpus"
        WALL_S=$(awk "BEGIN{printf \"%.3f\", $MS/1000}")
        SHA_SRC=""; SHA_RT=""; BYTE_IDENT=false
        EXPECTED_KIND="upload"; RC_OUT=$RC; PASSED=false
        emit_cell
        return 1
    fi

    # 2) Download large file: segments=1 baseline
    # `put -r SOURCE_DIR REMOTE_ROOT` uploads the CONTENTS of SOURCE_DIR
    # into REMOTE_ROOT (the `source/` prefix is stripped), so the remote
    # layout is REMOTE_ROOT/{small,medium,large}/...
    local proto_dl1="$rt_dir/segments-1"
    mkdir -p "$proto_dl1"
    log_info "[$proto] download large/64MiB.bin (segments=1, baseline)"
    cli_run "$RUN_DIR/${proto}_dl_seg1.log" \
        get -P "$profile_id" --segments 1 --parallel 1 \
        "$remote_root/large/64MiB.bin" "$proto_dl1/64MiB.bin"
    local rc1=$RC ms1=$MS
    local sha_src
    # source.sha256 entries look like:  <sha>  ./large/64MiB.bin
    sha_src=$(awk '$2 == "./large/64MiB.bin" {print $1}' "$SOURCE_DIR/../source.sha256")
    local sha1
    sha1=$(sha256sum "$proto_dl1/64MiB.bin" 2>/dev/null | awk '{print $1}')

    SURFACE="cli-segments-1"; PROTOCOL="$proto"; FILE_TAG="large/64MiB.bin"
    WALL_S=$(awk "BEGIN{printf \"%.3f\", $ms1/1000}")
    BASELINE_S="$WALL_S"
    SPEEDUP="1.000"
    BAND_FLOOR="null"; BAND_CEIL="null"
    SHA_SRC="$sha_src"; SHA_RT="$sha1"
    BYTE_IDENT=$([[ "$sha_src" == "$sha1" ]] && echo true || echo false)
    EXPECTED_KIND="LegacySingle"; RC_OUT=$rc1
    PASSED=$([[ "$rc1" -eq 0 && "$BYTE_IDENT" == "true" ]] && echo true || echo false)
    emit_cell

    # 3) Download large file: segments=4 (DAG range)
    local proto_dl4="$rt_dir/segments-4"
    mkdir -p "$proto_dl4"
    log_info "[$proto] download large/64MiB.bin (segments=4, DAG)"
    cli_run "$RUN_DIR/${proto}_dl_seg4.log" \
        get -P "$profile_id" --segments 4 --parallel 1 \
        "$remote_root/large/64MiB.bin" "$proto_dl4/64MiB.bin"
    local rc4=$RC ms4=$MS
    local sha4
    sha4=$(sha256sum "$proto_dl4/64MiB.bin" 2>/dev/null | awk '{print $1}')
    local speedup
    if [[ "$ms4" -gt 0 ]]; then
        speedup=$(awk "BEGIN{printf \"%.3f\", $ms1/$ms4}")
    else
        speedup="0.000"
    fi
    local bf bc
    read -r bf bc < <(band_for "$proto")
    local in_band="true"
    if awk "BEGIN{exit !($speedup < $bf)}" ; then in_band="false"; fi
    if awk "BEGIN{exit !($speedup > $bc)}" ; then in_band="false"; fi

    SURFACE="cli-segments-n"; PROTOCOL="$proto"; FILE_TAG="large/64MiB.bin"
    WALL_S=$(awk "BEGIN{printf \"%.3f\", $ms4/1000}")
    BASELINE_S=$(awk "BEGIN{printf \"%.3f\", $ms1/1000}")
    SPEEDUP="$speedup"
    BAND_FLOOR="$bf"; BAND_CEIL="$bc"
    SHA_SRC="$sha_src"; SHA_RT="$sha4"
    BYTE_IDENT=$([[ "$sha_src" == "$sha4" ]] && echo true || echo false)
    EXPECTED_KIND="DAG-Range"; RC_OUT=$rc4
    # GTC-0 baseline acceptance: byte-identity is mandatory; band is
    # recorded but not gated (we are *establishing* the baseline). It
    # becomes a gate from GTC-6 onward.
    PASSED=$([[ "$rc4" -eq 0 && "$BYTE_IDENT" == "true" ]] && echo true || echo false)
    emit_cell

    # 4) Roundtrip byte-identity for the full small bucket via batch
    # download (parallel=4 -> exercises ProviderTransferExecutor batch).
    local proto_batch="$rt_dir/parallel-4"
    mkdir -p "$proto_batch"
    log_info "[$proto] batch download small/* (parallel=4)"
    cli_run "$RUN_DIR/${proto}_dl_par4.log" \
        get -P "$profile_id" --parallel 4 -r \
        "$remote_root/small" "$proto_batch/small"
    local rcb=$RC msb=$MS

    # Per-file byte-identity for the small bucket
    local all_ok="true" mismatch=0 missing=0
    while read -r src_sha src_path; do
        # source.sha256 entries are "./small/file_XX.bin"
        case "$src_path" in
            ./small/*) ;;
            *) continue ;;
        esac
        local rel="${src_path#./small/}"
        local dst="$proto_batch/small/$rel"
        if [[ ! -f "$dst" ]]; then
            missing=$((missing + 1))
            all_ok="false"
            continue
        fi
        local got
        got=$(sha256sum "$dst" | awk '{print $1}')
        if [[ "$got" != "$src_sha" ]]; then
            mismatch=$((mismatch + 1))
            all_ok="false"
        fi
    done <"$SOURCE_DIR/../source.sha256"

    SURFACE="cli-parallel-n"; PROTOCOL="$proto"; FILE_TAG="small/*"
    WALL_S=$(awk "BEGIN{printf \"%.3f\", $msb/1000}")
    BASELINE_S="null"; SPEEDUP="null"
    BAND_FLOOR="null"; BAND_CEIL="null"
    SHA_SRC="(bucket)"; SHA_RT="(bucket)"
    BYTE_IDENT="$all_ok"
    EXPECTED_KIND="DAG-Batch"; RC_OUT=$rcb
    PASSED=$([[ "$rcb" -eq 0 && "$all_ok" == "true" ]] && echo true || echo false)
    emit_cell

    log_info "[$proto] mismatched=$mismatch missing=$missing"
    log_ok "[$proto] done: seg1=${ms1}ms seg4=${ms4}ms speedup=${speedup} band=${bf}..${bc}"

    cleanup_remote "$proto"
}

# ---------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------

OVERALL_PASS=0
OVERALL_FAIL=0
for proto in "${PROTO_LIST[@]}"; do
    if run_one_protocol "$proto"; then
        OVERALL_PASS=$((OVERALL_PASS + 1))
    else
        OVERALL_FAIL=$((OVERALL_FAIL + 1))
    fi
done

# ---------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------

python3 - "$RUN_DIR/cells.json" "$REPORT_JSON" "$REPORT_MD" "$RUN_ID" <<'PY'
import json, sys, statistics
cells_p, json_p, md_p, run_id = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
with open(cells_p) as f:
    cells = json.load(f)
passed = sum(1 for c in cells if c["passed"])
total = len(cells)
summary = {
    "run_id": run_id,
    "spec_version": "0.1.0",
    "cells": cells,
    "summary": {
        "total_cells": total,
        "passed": passed,
        "failed": total - passed,
    },
}
with open(json_p, "w") as f:
    json.dump(summary, f, indent=2)

lines = []
lines.append(f"# GTC parity report — {run_id}")
lines.append("")
lines.append(f"- spec: 0.1.0")
lines.append(f"- cells: {total} ({passed} passed, {total-passed} failed)")
lines.append("")
lines.append("| protocol | surface | file | wall (s) | baseline (s) | speedup | band | byte-id | exit | pass |")
lines.append("|---|---|---|---:|---:|---:|---|---|---:|---|")
for c in cells:
    band = f"{c['speedup_band_floor']}..{c['speedup_band_ceiling']}" if c["speedup_band_floor"] is not None else "-"
    sp   = c["speedup"] if c["speedup"] is not None else "-"
    bid  = "yes" if c["byte_identical"] else "**NO**"
    pas  = "ok" if c["passed"] else "**fail**"
    base = c["baseline_wall_clock_s"] if c["baseline_wall_clock_s"] is not None else "-"
    lines.append(f"| {c['protocol']} | {c['surface']} | {c['file']} | {c['wall_clock_s']:.3f} | {base} | {sp} | {band} | {bid} | {c['exit_code']} | {pas} |")
with open(md_p, "w") as f:
    f.write("\n".join(lines) + "\n")
PY

if [[ "$KEEP_CORPUS" != "1" ]]; then
    rm -rf "$CORPUS_DIR"
fi
if [[ -n "$BANDS_FILE" ]]; then
    rm -f "$BANDS_FILE"
fi

log_info "report.json: $REPORT_JSON"
log_info "report.md:   $REPORT_MD"

if [[ "$OVERALL_FAIL" -eq 0 ]]; then
    log_ok "harness GREEN: $OVERALL_PASS/$((OVERALL_PASS+OVERALL_FAIL)) protocols passed"
    exit 0
else
    log_err "harness RED: $OVERALL_FAIL/$((OVERALL_PASS+OVERALL_FAIL)) protocols failed"
    exit 1
fi
