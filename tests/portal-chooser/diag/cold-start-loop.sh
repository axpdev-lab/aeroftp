#!/usr/bin/env bash
# DIAGNOSTIC ONLY (branch diag/portal-chooser-cold-start-stacks, never merged).
# N cold starts through recon-session.sh, rotating the three gate scenarios,
# odd launches with the WebKit remote inspector on. A miss runs diag/on-miss.sh.
set -uo pipefail
APP="$1"; N="${2:-20}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${DIAG_OUT:-$PWD/cold-start-diag}"; mkdir -p "$OUT"
miss=0
for i in $(seq 1 "$N"); do
  mode=cancel; np=0
  case $((i % 3)) in 1) mode=cancel ;; 2) mode=error ;; 0) np=1 ;; esac
  insp=""; [ $((i % 2)) -eq 1 ] && insp="WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9222"
  d="$OUT/$i"; start=$(date +%s)
  env $insp RECON_OUT="$d" RECON_MODE="$mode" RECON_NO_PORTAL="$np" RECON_WAIT_SECONDS=25 \
      RECON_SETTLE_SECONDS=0 RECON_ON_MISS_HOOK="$HERE/diag/on-miss.sh" \
      "$HERE/recon-session.sh" "$APP" >"$d.log" 2>&1
  rc=$?
  ready=0; grep -q "app_ready seen: 1" "$d.log" && ready=1
  [ "$ready" -eq 0 ] && miss=$((miss + 1))
  # Keep the artefact small: a passing launch keeps only its app log.
  if [ "$ready" -eq 1 ]; then find "$d" -type f ! -name app.log -delete; fi
  echo "launch $i mode=$mode noportal=$np inspector=${insp:+on}${insp:-off} rc=$rc ready=$ready secs=$(( $(date +%s) - start ))" | tee -a "$OUT/summary.txt"
done
echo "missed app_ready: $miss/$N" | tee -a "$OUT/summary.txt"
