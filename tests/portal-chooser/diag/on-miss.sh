#!/usr/bin/env bash
# DIAGNOSTIC ONLY (branch diag/portal-chooser-cold-start-stacks, never merged).
# Called by recon-session.sh when the app missed app_ready, BEFORE it is killed.
# Captures what a hung cold start is actually waiting on.
APP_PID="$1"; OUT="$2"; HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
set +e
echo "=== host: nproc=$(nproc) kernel=$(uname -r) user=$(id)"
ls -la /dev/dri 2>&1
desc() { echo "$1"; for c in $(ps -o pid= --ppid "$1" 2>/dev/null); do desc "$c"; done; }
PIDS="$(desc "$APP_PID" | tr '\n' ' ')"
echo "=== process tree under $APP_PID: $PIDS"
ps -o pid,ppid,stat,etimes,nlwp,wchan:32,args -p "$(echo $PIDS | tr ' ' ,)" 2>&1
for p in $PIDS; do
  comm="$(cat /proc/$p/comm 2>/dev/null)"
  echo "--- threads of $p ($comm)"
  for t in /proc/$p/task/*; do
    printf '%s %s %s\n' "$(cat $t/comm 2>/dev/null)" "$(awk '{print $3}' $t/stat 2>/dev/null)" "$(sudo cat $t/wchan 2>/dev/null)"
  done | sort | uniq -c | sort -rn
done
echo "=== sockets on 14321 (Recv-Q / Send-Q)"
sudo ss -tanpi 2>/dev/null | grep -A1 ":14321" | grep -v "^--"
echo "=== inspector"
if [ -n "${WEBKIT_INSPECTOR_HTTP_SERVER:-}" ]; then
  timeout 40 node "$HERE/inspect-page.mjs" "$WEBKIT_INSPECTOR_HTTP_SERVER" 15000 >"$OUT/inspector.json" 2>&1
  echo "inspector rc=$? (see inspector.json)"
else
  echo "inspector off for this launch"
fi
echo "=== screenshot"
xwd -root -silent 2>/dev/null | convert xwd:- "png:$OUT/miss-screen.png" 2>/dev/null && echo saved
echo "=== gdb stacks"
for p in $PIDS; do
  comm="$(cat /proc/$p/comm 2>/dev/null)"
  case "$comm" in
    aeroftp*|WebKitWebProces|WebKitNetworkPr|WebKitGPUProces)
      timeout 900 sudo env DEBUGINFOD_URLS=https://debuginfod.ubuntu.com DEBUGINFOD_CACHE_PATH=/var/tmp/debuginfod \
        gdb -q -batch -p "$p" -iex 'set debuginfod enabled on' -ex 'set pagination off' \
        -ex 'info threads' -ex 'thread apply all bt 40' >"$OUT/gdb-$comm-$p.txt" 2>&1
      echo "gdb $p $comm rc=$? lines=$(wc -l <"$OUT/gdb-$comm-$p.txt")"
      ;;
  esac
done
echo "=== done"
