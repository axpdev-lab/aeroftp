#!/usr/bin/env bash
# Gate G3: launch the confined GUI on a disposable X server and prove two
# things about it — the React frontend reached app_ready, and WebKit actually
# painted the window.  A live process is insufficient: the blank WebKit
# regression in #462 stayed alive until the splash safety timeout, and a
# process that merely reports "ready" would still let a white window ship.
set -euo pipefail

APP="${1:-/snap/bin/aeroftp}"
WAIT_SECONDS="${SNAP_GUI_WAIT_SECONDS:-40}"
# WebKit paints a frame or two after the window is mapped; give it room before
# the screenshot so a slow first paint is not read as a blank window.
SETTLE_SECONDS="${SNAP_GUI_SETTLE_SECONDS:-4}"
# A blank frame is a handful of distinct colours (X root background plus one
# flat WebKit page).  A rendered AeroFTP frame — themed chrome, icons,
# antialiased text — is in the thousands.  The floor sits far from both.
MIN_COLORS="${SNAP_GUI_MIN_COLORS:-64}"
SHOT="${SNAP_GUI_SCREENSHOT:-$PWD/snap-gui-screenshot.png}"

for tool in Xvfb xwd setsid grep; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "::error::snap-gui-check needs '$tool' (apt: xvfb x11-apps util-linux grep)" >&2
    exit 2
  }
done
# ImageMagick 7 renamed the CLI; accept either entry point.
if command -v magick >/dev/null 2>&1; then
  IM=magick
elif command -v convert >/dev/null 2>&1; then
  IM=convert
else
  echo "::error::snap-gui-check needs ImageMagick (apt: imagemagick)" >&2
  exit 2
fi
[ -x "$APP" ] || {
  echo "::error::GUI launcher is missing or not executable: $APP" >&2
  exit 2
}

WORK="$(mktemp -d)"
LOG="$WORK/aeroftp-gui.log"
PIDFILE="$WORK/app.pid"
APP_PID=""
XVFB_PID=""

cleanup() {
  if [ -n "$APP_PID" ]; then
    kill -TERM -- "-$APP_PID" 2>/dev/null || true
    sleep 1
    kill -KILL -- "-$APP_PID" 2>/dev/null || true
  fi
  if [ -n "$XVFB_PID" ]; then
    kill -TERM "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# Own the X server instead of delegating to xvfb-run: the display number has to
# be known here so the frame can be captured off it.  Strict snaps also have a
# private /tmp and cannot read the Xauthority cookie xvfb-run generates, so
# access control is disabled — this server is disposable and only reachable
# from inside the runner.
DISPLAY_NUM=""
for candidate in $(seq 90 130); do
  if [ ! -e "/tmp/.X11-unix/X$candidate" ] && [ ! -e "/tmp/.X$candidate-lock" ]; then
    DISPLAY_NUM="$candidate"
    break
  fi
done
[ -n "$DISPLAY_NUM" ] || {
  echo "::error::no free X display between :90 and :130" >&2
  exit 2
}

echo "Starting Xvfb on :$DISPLAY_NUM ..."
Xvfb ":$DISPLAY_NUM" -screen 0 1600x1000x24 -ac >"$WORK/xvfb.log" 2>&1 &
XVFB_PID=$!
# Probe the server the same way the capture will use it, rather than trusting
# the socket to appear: a listening socket is not yet a server that answers.
server_up=false
for _ in $(seq 1 50); do
  if xwd -display ":$DISPLAY_NUM" -root -silent >/dev/null 2>&1; then
    server_up=true
    break
  fi
  kill -0 "$XVFB_PID" 2>/dev/null || break
  sleep 0.2
done
if [ "$server_up" != true ]; then
  echo "::error::Xvfb did not come up on :$DISPLAY_NUM" >&2
  sed -n '1,40p' "$WORK/xvfb.log" >&2
  exit 2
fi

echo "Launching the confined GUI (timeout: ${WAIT_SECONDS}s)..."
# The wrapper records its own pid before exec'ing the app, so the pid below is
# the session leader the whole process group can be signalled through.
DISPLAY=":$DISPLAY_NUM" setsid bash -c 'echo $$ >"$1"; exec "$2"' _ "$PIDFILE" "$APP" \
  >"$LOG" 2>&1 &
for _ in $(seq 1 50); do
  [ -s "$PIDFILE" ] && break
  sleep 0.2
done
APP_PID="$(cat "$PIDFILE" 2>/dev/null || true)"
[ -n "$APP_PID" ] || {
  echo "::error::the confined GUI never started" >&2
  sed -n '1,80p' "$LOG" >&2
  exit 1
}

# `app_ready post-show` is logged in the same main-thread callback as show(),
# and GTK/X11 map the window asynchronously — on this platform that line always
# reads is_visible=false.  The truth lands in the +300ms/+1200ms re-heal
# diagnostics, so accept visibility from any app_ready diagnostic.  The prefix
# still carries the real assertion: those lines only exist once the frontend
# has invoked the app_ready command (src/App.tsx), so a blank page that never
# boots React can never produce them.
ready=false
for ((second = 1; second <= WAIT_SECONDS; second++)); do
  if grep -qE '\[diag #290\] app_ready [^:]+: is_visible=true' "$LOG"; then
    ready=true
    break
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    break
  fi
  sleep 1
done

# Capture before asserting: a failed run needs the frame even more than a green
# one does.
sleep "$SETTLE_SECONDS"
captured=false
if xwd -display ":$DISPLAY_NUM" -root -silent >"$WORK/shot.xwd" 2>"$WORK/xwd.err" &&
  "$IM" "xwd:$WORK/shot.xwd" "png:$SHOT" 2>"$WORK/im.err"; then
  captured=true
else
  echo "screenshot capture failed:" >&2
  sed -n '1,20p' "$WORK/xwd.err" "$WORK/im.err" >&2 || true
fi

echo "--- confined GUI log ---"
sed -n '1,240p' "$LOG"

if grep -qE "EGL_BAD_(PARAMETER|DISPLAY)|Could not create default EGL display|libEGL fatal" "$LOG"; then
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

if [ "$captured" != true ]; then
  echo "::error::could not capture the GUI frame, so blankness cannot be ruled out" >&2
  exit 1
fi
colors="$("$IM" "png:$SHOT" -format %k info: 2>/dev/null | tr -dc '0-9')"
if [ -z "$colors" ]; then
  echo "::error::could not measure the captured frame, so blankness cannot be ruled out" >&2
  exit 1
fi
echo "Captured frame: $SHOT (${colors} distinct colours)"
if [ "$colors" -lt "$MIN_COLORS" ]; then
  echo "::error::the snap GUI window is blank: only ${colors} distinct colours on screen (floor: ${MIN_COLORS})" >&2
  exit 1
fi

echo "OK: confined snap GUI reached app_ready, became visible and painted a real frame under Xvfb."
