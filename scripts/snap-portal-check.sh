#!/usr/bin/env bash
# Gate G5: the desktop portal must let the installed snap reach the network.
#
# The GUI loads its UI from http://127.0.0.1, and inside a snap GLib asks the
# desktop portal (org.freedesktop.portal.ProxyResolver) how to reach that URL.
# xdg-desktop-portal answers a snap only when snapd reports it has network
# access, which it reads from `snap routine portal-info` as HasNetworkStatus,
# and snapd sets that only for a snap that plugs `network-status`. Without it
# the portal refuses with "This call is not available inside the sandbox",
# WebKit shows that error as the page, and the app never starts. Gate G3 cannot
# see this: the runner has no portal, so GLib falls back to a direct resolver
# and the window loads.
#
# This asks snapd the same question the portal asks, for a process actually
# running inside the snap's confinement.
set -euo pipefail

SNAP_NAME="${1:-aeroftp}"
WAIT_SECONDS="${SNAP_PORTAL_WAIT_SECONDS:-20}"

WORK="$(mktemp -d)"
PIDFILE="$WORK/confined.pid"
PID=""

cleanup() {
  if [ -n "$PID" ]; then
    kill -TERM "$PID" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 "$PID" 2>/dev/null || break
      sleep 0.2
    done
    kill -KILL "$PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# `snap run` reaches the command through exec alone, so the pid recorded here
# becomes the confined `sleep`. Wait for that last exec, not just for the
# label: snap-confine already carries the label and does not die on TERM.
# The sleep is short because the snap's AppArmor profile takes signals only
# from unconfined peers: under any other profile the kill below is refused
# and the process has to end by itself.
setsid bash -c 'echo $$ >"$1"; exec snap run --shell "$2" -c "exec sleep 60"' \
  _ "$PIDFILE" "$SNAP_NAME" >"$WORK/run.log" 2>&1 &

label=""
comm=""
for ((i = 0; i < WAIT_SECONDS * 5; i++)); do
  PID="$(cat "$PIDFILE" 2>/dev/null || true)"
  if [ -n "$PID" ]; then
    label="$(cat "/proc/$PID/attr/current" 2>/dev/null || true)"
    comm="$(cat "/proc/$PID/comm" 2>/dev/null || true)"
    case "$label" in
      "snap.$SNAP_NAME."*) [ "$comm" = sleep ] && break ;;
    esac
  fi
  sleep 0.2
done
case "$label:$comm" in
  "snap.$SNAP_NAME."*:sleep) ;;
  *)
    echo "::error::no process of the snap '$SNAP_NAME' started inside its confinement within ${WAIT_SECONDS}s (label: '${label:-none}', process: '${comm:-none}')" >&2
    sed -n '1,40p' "$WORK/run.log" >&2
    exit 1
    ;;
esac

info="$(snap routine portal-info "$PID" 2>&1)" || {
  echo "::error::snapd could not describe the confined process $PID to the portal" >&2
  echo "$info" >&2
  exit 1
}
echo "--- snap routine portal-info $PID ---"
echo "$info"

if ! grep -qx "InstanceName=$SNAP_NAME" <<<"$info"; then
  echo "::error::portal-info does not name the snap '$SNAP_NAME'" >&2
  exit 1
fi
if ! grep -qx 'HasNetworkStatus=true' <<<"$info"; then
  echo "::error::the desktop portal will refuse this snap's proxy and network-status lookups (HasNetworkStatus is not true): plug 'network-status' in snap/snapcraft.yaml, or the GUI cannot load its UI on a desktop" >&2
  exit 1
fi

echo "OK: snapd tells the desktop portal that '$SNAP_NAME' has network access."
