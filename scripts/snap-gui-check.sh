#!/usr/bin/env bash
# Gate G3: launch the confined GUI on a disposable X server and prove that the
# React frontend reached app_ready.  A live process is insufficient: the blank
# WebKit regression in #462 stayed alive until the splash safety timeout.
set -euo pipefail

APP="${1:-/snap/bin/aeroftp}"
WAIT_SECONDS="${SNAP_GUI_WAIT_SECONDS:-25}"

for tool in xvfb-run xauth setsid grep; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "::error::snap-gui-check needs '$tool' (apt: xvfb xauth util-linux grep)" >&2
    exit 2
  }
done
[ -x "$APP" ] || {
  echo "::error::GUI launcher is missing or not executable: $APP" >&2
  exit 2
}

WORK="$(mktemp -d)"
LOG="$WORK/aeroftp-gui.log"
PID=""

cleanup() {
  if [ -n "$PID" ]; then
    kill -TERM -- "-$PID" 2>/dev/null || true
    sleep 1
    kill -KILL -- "-$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

echo "Launching the confined GUI under Xvfb (timeout: ${WAIT_SECONDS}s)..."
setsid xvfb-run -a -s "-screen 0 1600x1000x24" "$APP" >"$LOG" 2>&1 &
PID=$!

ready=false
for ((second = 1; second <= WAIT_SECONDS; second++)); do
  if grep -q "app_ready post-show: is_visible=true" "$LOG"; then
    ready=true
    break
  fi
  if ! kill -0 "$PID" 2>/dev/null; then
    break
  fi
  sleep 1
done

echo "--- confined GUI log ---"
sed -n '1,240p' "$LOG"

if grep -qE "EGL_BAD_(PARAMETER|DISPLAY)|Could not create default EGL display" "$LOG"; then
  echo "::error::WebKit could not initialize EGL inside the snap" >&2
  exit 1
fi
if grep -q "Splash screen safety timeout reached" "$LOG"; then
  echo "::error::the frontend never signalled app_ready" >&2
  exit 1
fi
if [ "$ready" != true ]; then
  echo "::error::the snap GUI did not reach a visible app_ready state within ${WAIT_SECONDS}s" >&2
  exit 1
fi

echo "OK: confined snap GUI reached app_ready and became visible under Xvfb."
