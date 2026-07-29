#!/usr/bin/env bash
# Reconnaissance, not a test. It answers one question the second half of #464
# cannot be written without: from an automated session, how is AeroFTP's file
# chooser actually reachable?
#
# Nothing here asserts. It launches the real app under an isolated X and D-Bus
# session with the portal stand-in in place, then records everything that could
# make a trigger deterministic:
#
#   - the AT-SPI accessibility tree, which is where a name-based trigger would
#     come from and is far more stable than clicking coordinates;
#   - the window geometry and a screenshot, for the coordinate fallback;
#   - the app's own log, including whether it set GTK_USE_PORTAL;
#   - the portal's call record, which will be EMPTY on this run because nothing
#     opens a picker yet - that is expected and is the baseline.
#
# It is deliberately not a gate. A "chooser test" that launches the app, reaches
# no picker and passes anyway is the vacuous green #464 exists to prevent.
set -uo pipefail

APP="${1:?usage: recon-session.sh /path/to/aeroftp}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${RECON_OUT:-$PWD/recon-out}"
PORTAL_BIN="$HERE/fake-portal/target/release/aeroftp-fake-portal"
WAIT_SECONDS="${RECON_WAIT_SECONDS:-45}"
SETTLE_SECONDS="${RECON_SETTLE_SECONDS:-6}"

mkdir -p "$OUT"
[ -x "$APP" ] || { echo "::error::app binary missing or not executable: $APP" >&2; exit 2; }

# A binary built with a bare `cargo build` does NOT embed the frontend: it points
# at the Vite dev server and comes up as a white window saying "Could not connect
# to 127.0.0.1". Two recon runs were burned on that before it was spotted, and
# one of them overwrote a good release artifact. Refuse rather than repeat it.
if strings "$APP" 2>/dev/null | grep -q "127.0.0.1:5173/splash.html"; then
  echo "::error::$APP was built without the Tauri CLI and points at the dev server." >&2
  echo "  It will show a blank window and no frontend. Build it with:" >&2
  echo "    npm run tauri build -- --no-bundle" >&2
  exit 2
fi
[ -x "$PORTAL_BIN" ] || { echo "::error::portal stand-in missing: $PORTAL_BIN" >&2; exit 2; }

for tool in Xvfb xwd dbus-run-session; do
  command -v "$tool" >/dev/null 2>&1 || { echo "::error::recon needs '$tool'" >&2; exit 2; }
done
IM=""
command -v magick >/dev/null 2>&1 && IM=magick
[ -z "$IM" ] && command -v convert >/dev/null 2>&1 && IM=convert

# Own the X server rather than delegating to xvfb-run: the display number has to
# be known here so the frame can be captured off it. Same reasoning as
# scripts/snap-gui-check.sh, which already does this for gate G3.
DISPLAY_NUM=""
for candidate in $(seq 90 130); do
  if [ ! -e "/tmp/.X11-unix/X$candidate" ] && [ ! -e "/tmp/.X$candidate-lock" ]; then
    DISPLAY_NUM="$candidate"
    break
  fi
done
[ -n "$DISPLAY_NUM" ] || { echo "::error::no free X display" >&2; exit 2; }

# Force the toolkit onto OUR X display, and refuse to run if it could still
# reach the real one.
#
# This is the single most important thing in this file, and it is here because it
# was got wrong: on a Wayland desktop the environment carries WAYLAND_DISPLAY and
# GDK_BACKEND=wayland, and GTK prefers Wayland over DISPLAY. Setting DISPLAY to a
# private Xvfb therefore isolates NOTHING -- every launched window opened on the
# developer live session, where it could be clicked, while the harness captured a
# blank frame from an Xvfb nobody was drawing on. The blank frame was the evidence
# and it was misread as a capture artefact.
unset WAYLAND_DISPLAY
export GDK_BACKEND=x11
if [ -n "${WAYLAND_DISPLAY:-}" ] || [ "${GDK_BACKEND:-}" != "x11" ]; then
  echo "::error::refusing to launch: the toolkit could still reach a Wayland session" >&2
  exit 2
fi

Xvfb ":$DISPLAY_NUM" -screen 0 "${RECON_SCREEN:-1920x1200}x24" -nolisten tcp >"$OUT/xvfb.log" 2>&1 &
XVFB_PID=$!
export DISPLAY=":$DISPLAY_NUM"

cleanup() {
  [ -n "${DBUS_PID:-}" ] && kill -TERM "$DBUS_PID" 2>/dev/null
  [ -n "${APP_PID:-}" ] && { kill -TERM -- "-$APP_PID" 2>/dev/null; sleep 1; kill -KILL -- "-$APP_PID" 2>/dev/null; }
  [ -n "${PORTAL_PID:-}" ] && kill -INT "$PORTAL_PID" 2>/dev/null
  [ -n "${XVFB_PID:-}" ] && kill -TERM "$XVFB_PID" 2>/dev/null
  wait 2>/dev/null
}
trap cleanup EXIT

for _ in $(seq 1 50); do
  [ -e "/tmp/.X11-unix/X$DISPLAY_NUM" ] && break
  sleep 0.2
done

# Everything from here runs on a private session bus, so the stand-in owning
# org.freedesktop.portal.Desktop cannot touch anything outside this job.
# A private config/data/cache root. Without this the app would read and write the
# developer's real vault and profile store while being driven by a test, which
# is not a risk worth taking for a reconnaissance run.
RECON_HOME="$(mktemp -d)"
RECON_RUNTIME="$RECON_HOME/runtime"
mkdir -p "$RECON_RUNTIME"
chmod 700 "$RECON_RUNTIME"
export RECON_RUNTIME
export XDG_CONFIG_HOME="$RECON_HOME/config"
export XDG_DATA_HOME="$RECON_HOME/data"
export XDG_CACHE_HOME="$RECON_HOME/cache"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
echo "isolated config root: $RECON_HOME"

export RECON_OUT="$OUT" APP="$APP" PORTAL_BIN="$PORTAL_BIN" HERE="$HERE"
export RECON_MODE="${RECON_MODE:-cancel}" RECON_NO_PORTAL="${RECON_NO_PORTAL:-0}"
export WAIT_SECONDS SETTLE_SECONDS IM
export XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME
# Our own session bus, from a config with NO service directories, instead of
# dbus-run-session. Not a refinement: dbus-run-session inherits the system
# service dirs, so when the stand-in was deliberately absent the REAL
# xdg-desktop-portal was activated on demand and answered in its place, quietly
# turning "no portal installed" into "the real portal on a private bus". With
# activation off, the two cases differ only by whether the stand-in is running,
# which is what makes the comparison mean anything.
# RECON_NO_PORTAL builds a bus that can activate everything EXCEPT the portal, so
# the run models a host where xdg-desktop-portal is simply not installed rather
# than a host where nothing at all works.
BUS_CONF="$HERE/private-bus.conf"
if [ "${RECON_NO_PORTAL:-0}" = "1" ]; then
  SVC_DIR="$RECON_HOME/services"
  "$HERE/make-servicedir-without-portal.sh" "$SVC_DIR"
  BUS_CONF="$RECON_HOME/bus-noportal.conf"
  sed "s#<standard_session_servicedirs/>#<servicedir>$SVC_DIR</servicedir>#" \
      "$HERE/private-bus.conf" >"$BUS_CONF"
fi

DBUS_OUT="$(mktemp)"
# --print-address=1 --print-pid=1, not the bare flags: given bare, dbus-daemon
# reads the NEXT argument as the file descriptor and dies with
# "Invalid file descriptor: --print-pid".
dbus-daemon --config-file="$BUS_CONF" \
    --fork --print-address=1 --print-pid=1 >"$DBUS_OUT" 2>"$OUT/dbus.err" || {
  echo "::error::could not start the private session bus" >&2
  cat "$OUT/dbus.err" >&2
  exit 2
}
DBUS_ADDR="$(sed -n 1p "$DBUS_OUT")"
DBUS_PID="$(sed -n 2p "$DBUS_OUT")"
rm -f "$DBUS_OUT"
[ -n "$DBUS_ADDR" ] || { echo "::error::private bus produced no address" >&2; exit 2; }
export DBUS_SESSION_BUS_ADDRESS="$DBUS_ADDR"
echo "private session bus: $DBUS_ADDR (pid $DBUS_PID, activation disabled)"

bash -uo pipefail -c '
  OUT="$RECON_OUT"
  # RECON_NO_PORTAL leaves org.freedesktop.portal.Desktop unowned, which is the
  # "portal unavailable" case: a host with no xdg-desktop-portal installed at all.
  # That is a different situation from a portal that answers with an error, and
  # the code comment in lib.rs makes a claim about it worth checking.
  if [ "${RECON_NO_PORTAL:-0}" = "1" ]; then
    echo "NO portal started: org.freedesktop.portal.Desktop is unowned on this bus"
    PORTAL_PID=""
    touch "$OUT/portal-ready"
  else
  "$PORTAL_BIN" --mode "${RECON_MODE:-cancel}" --log "$OUT/portal-calls.jsonl" \
      --ready-file "$OUT/portal-ready" --accept-path "$OUT" \
      >"$OUT/portal.out" 2>"$OUT/portal.err" &
  PORTAL_PID=$!
  fi
  for _ in $(seq 1 100); do [ -f "$OUT/portal-ready" ] && break; sleep 0.1; done
  if [ ! -f "$OUT/portal-ready" ]; then
    echo "::error::the portal stand-in never became ready" >&2
    cat "$OUT/portal.err" >&2
    exit 1
  fi
  echo "portal stand-in is up and owns org.freedesktop.portal.Desktop"

  # Record what the bus looks like, so a later trigger can tell a real portal
  # from ours if the runner image ever ships one.
  { busctl --user list 2>/dev/null || gdbus call --session \
      --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
      --method org.freedesktop.DBus.ListNames 2>/dev/null; } >"$OUT/bus-names.txt"

  # A private accessibility bus, started and verified rather than assumed.
  # dbus-run-session isolates the session bus only; see start-private-a11y.sh for
  # why that is not enough and what went wrong without it.
  . "$HERE/start-private-a11y.sh" || exit 1

  # Accessibility has to be on BEFORE the app starts: WebKitGTK builds its
  # accessible tree at widget construction, so enabling it afterwards yields an
  # empty tree and would read as "the app exposes nothing".
  export GTK_MODULES="${GTK_MODULES:+$GTK_MODULES:}gail:atk-bridge"
  export NO_AT_BRIDGE=0
  export GTK_USE_PORTAL=1

  # Pin the locale. The accessible name of a control is its visible label, so on
  # an Italian host the tree comes back with "Chiudi" where a CI runner would say
  # "Close" -- a trigger keyed on either one is green on one machine and broken
  # on the other, for a reason nobody would look for. Observed exactly that on
  # the first recon run, which mixed both languages in one tree.
  if [ "${RECON_PIN_LOCALE:-1}" = "1" ]; then
    export LANG=en_US.UTF-8 LANGUAGE=en_US:en LC_ALL=C.UTF-8
  fi

  setsid "$APP" >"$OUT/app.log" 2>&1 &
  APP_PID=$!
  echo "app pid $APP_PID"

  ready=0
  for _ in $(seq 1 "$WAIT_SECONDS"); do
    grep -q "app_ready" "$OUT/app.log" 2>/dev/null && { ready=1; break; }
    kill -0 "$APP_PID" 2>/dev/null || break
    sleep 1
  done
  echo "app_ready seen: $ready"
  sleep "$SETTLE_SECONDS"

  # Positive proof that the window is on OUR display. Absence of evidence is not
  # evidence of isolation: the previous version of this harness inferred
  # isolation from a probe that was silently failing, and from a blank frame that
  # actually meant the app had opened somewhere else entirely.
  own_windows=$(xdotool search --class aeroftp 2>/dev/null | wc -l)
  echo "windows with class aeroftp on $DISPLAY: $own_windows"
  if [ "$own_windows" -eq 0 ]; then
    echo "::error::the app is running but has NO window on $DISPLAY." >&2
    echo "  It has opened somewhere else - almost certainly the real session." >&2
    echo "  Stopping before anything is driven." >&2
    kill -TERM -- "-$APP_PID" 2>/dev/null
    exit 1
  fi

  # Windows and geometry, for a coordinate-based fallback trigger.
  { xdotool search --onlyvisible --name "." getwindowname %@ 2>/dev/null; } >"$OUT/windows.txt" 2>&1 || true
  { xdotool search --onlyvisible --name "." 2>/dev/null | while read -r w; do
      echo "--- window $w ---"
      xdotool getwindowname "$w" 2>/dev/null
      xdotool getwindowgeometry "$w" 2>/dev/null
    done; } >>"$OUT/windows.txt" 2>&1 || true

  xwd -root -silent >"$OUT/screen.xwd" 2>/dev/null || true
  if [ -n "${IM:-}" ] && [ -s "$OUT/screen.xwd" ]; then
    "$IM" "$OUT/screen.xwd" "$OUT/screen.png" 2>/dev/null || true
    echo "distinct colours: $("$IM" "$OUT/screen.xwd" -format %k info: 2>/dev/null || echo unknown)" >"$OUT/screen-colours.txt"
  fi

  # The prize: the accessible tree. A name-based trigger built on this is stable
  # against layout changes in a way that clicking coordinates never is.
  if command -v python3 >/dev/null 2>&1; then
    python3 "$HERE/dump-atspi.py" >"$OUT/atspi-tree.txt" 2>"$OUT/atspi-err.txt" || true
  fi

  # Empirical sweep: press each candidate and see which one actually reaches the
  # portal. Reading the source to guess which button opens a chooser is slower
  # and less reliable than asking the running app, and the portal call record
  # is an unambiguous answer.
  # Fail closed BEFORE pressing anything. If this bus is not ours, stop.
  if ! python3 "$HERE/assert-private-a11y.py" >"$OUT/a11y-assert.txt" 2>&1; then
    cat "$OUT/a11y-assert.txt" >&2
    echo "::error::refusing to drive the UI: the accessibility bus is not private" >&2
    kill -TERM -- "-$APP_PID" 2>/dev/null
    exit 1
  fi
  cat "$OUT/a11y-assert.txt"

  if [ -n "${RECON_PRESS:-}" ]; then
    IFS="|" read -ra CANDIDATES <<< "$RECON_PRESS"
    for cand in "${CANDIDATES[@]}"; do
      before=$(wc -l < "$OUT/portal-calls.jsonl" 2>/dev/null || echo 0)
      echo "--- pressing: $cand"
      python3 "$HERE/press-atspi.py" "$cand"; press_rc=$?
      sleep 2
      after=$(wc -l < "$OUT/portal-calls.jsonl" 2>/dev/null || echo 0)
      echo "    press_rc=$press_rc  portal calls: $before -> $after"
      if [ "$after" -gt "$before" ]; then
        echo "    *** THIS ONE REACHES THE PORTAL ***"
      fi
    done
  fi

  # Full tree AFTER the presses. --list only prints pressable named controls, so
  # a dialog that appeared would not show up there; this is what tells a fallback
  # chooser from nothing at all.
  if [ -n "${RECON_PRESS:-}" ]; then
    sleep "${RECON_POST_SETTLE:-3}"
    python3 "$HERE/dump-atspi.py" >"$OUT/atspi-tree-after.txt" 2>&1 || true
    echo "--- tree after presses: $(wc -l < "$OUT/atspi-tree-after.txt") nodes ---"
    # Window count is the discriminator the accessible tree cannot give: a native
    # GTK chooser is a separate toplevel, so it shows up here even if it never
    # reaches the a11y bus.
    {
      echo "--- toplevel windows after presses ---"
      for w in $(xdotool search --onlyvisible "" 2>/dev/null); do
        printf "%s\t%s\t%s\n" "$w" "$(xdotool getwindowname "$w" 2>/dev/null)" \
          "$(xdotool getwindowgeometry --shell "$w" 2>/dev/null | tr "\n" " ")"
      done
    } >"$OUT/windows-after.txt" 2>&1
    echo "--- visible windows after presses: $(grep -c . "$OUT/windows-after.txt") ---"
  fi

  echo "--- portal calls recorded during recon (expected: none yet) ---"
  cat "$OUT/portal-calls.jsonl" 2>/dev/null || echo "(none)"

  kill -TERM -- "-$APP_PID" 2>/dev/null
  sleep 1
  [ -n "${PORTAL_PID:-}" ] && kill -INT "$PORTAL_PID" 2>/dev/null
  [ -n "${PORTAL_PID:-}" ] && wait "$PORTAL_PID" 2>/dev/null
  portal_rc=$?
  echo "portal exit: $portal_rc  (3 = never asked, which is the expected baseline here)"
  # The a11y launcher is ours and must not outlive the session. Three fake-portal
  # processes were orphaned this way earlier: the inner shell exited, the parent
  # cleanup only knew about the app and Xvfb, and they kept running.
  kill -TERM "${A11Y_PID:-0}" 2>/dev/null
  wait "${A11Y_PID:-0}" 2>/dev/null
'
rc=$?

echo "== recon artefacts in $OUT =="
ls -la "$OUT" || true
echo
echo "== app log, first 60 lines =="
sed -n '1,60p' "$OUT/app.log" 2>/dev/null || echo "(no app log)"
echo
echo "== accessible tree, first 80 lines =="
sed -n '1,80p' "$OUT/atspi-tree.txt" 2>/dev/null || echo "(no tree; see atspi-err.txt)"

exit $rc
