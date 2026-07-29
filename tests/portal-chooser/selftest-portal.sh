#!/usr/bin/env bash
# Self-test for the fake portal, with no X server and no AeroFTP involved.
#
# It exists because the fake portal is the instrument the real test measures
# with, and an instrument that fails in the same direction as the bug it looks
# for is worse than no instrument: a stub that silently never delivers its
# Response signal would make the app hang, and that hang would be read as a
# defect in AeroFTP.
#
# The client is `portal-probe`, which subscribes the way GTK subscribes rather
# than watching the bus. That distinction is not pedantic: the first version of
# this file used `gdbus monitor`, which needs a `--dest` and, filtered on the
# well-known name, does not see a signal the portal emits under its unique name.
# It reported "no Response" for three modes that were all working. Observing a
# message on the bus would also not prove the thing that matters, which is that
# a subscriber RECEIVES it.
#
# Cases, each with a result known in advance:
#   1. cancel  -> Response(1) on the caller-predicted Request path
#   2. accept  -> Response(0) carrying the uris array, so cancel cannot pass by
#                 doing nothing
#   3. error   -> the method call itself fails, which is what a portal that is
#                 present but refusing looks like to the caller
#   4. unused  -> the portal exits 3 when nothing ever asked it, which is the
#                 verdict that turns "the app never used the portal" from a
#                 silent pass into a failure
#   5. close   -> Close() on the handle the caller was HANDED reaches an
#                 implementation, which the other four cases all pass without
#
# Requires: dbus-run-session. Deliberately NOT Xvfb: this half must stay runnable
# on any machine and in any CI job, including ones with no display packages.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${BIN_DIR:-$HERE/fake-portal/target/release}"
PORTAL_BIN="$BIN_DIR/aeroftp-fake-portal"
PROBE_BIN="$BIN_DIR/portal-probe"

command -v dbus-run-session >/dev/null 2>&1 || {
  echo "::error::selftest-portal needs 'dbus-run-session' (apt: dbus)" >&2
  exit 2
}

if [ ! -x "$PORTAL_BIN" ] || [ ! -x "$PROBE_BIN" ]; then
  echo "building the fake portal and probe first..." >&2
  (cd "$HERE/fake-portal" && cargo build --release >/dev/null)
fi
for b in "$PORTAL_BIN" "$PROBE_BIN"; do
  [ -x "$b" ] || { echo "::error::missing harness binary: $b" >&2; exit 2; }
done

PASS=0
FAIL=0
ok()  { printf '  ok   %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }

# One throwaway session bus per case. `dbus-run-session` sets
# DBUS_SESSION_BUS_ADDRESS and tears the bus down on exit, so nothing leaks onto
# the developer's real session -- which matters here, because owning
# org.freedesktop.portal.Desktop on a live desktop would hijack the file chooser
# of every running application.
run_case() {
  local mode="$1" probe_args="$2"
  local work; work="$(mktemp -d)"
  MODE="$mode" WORK="$work" PROBE_ARGS="$probe_args" \
  PORTAL_BIN="$PORTAL_BIN" PROBE_BIN="$PROBE_BIN" \
  dbus-run-session -- bash -uo pipefail -c '
    log="$WORK/calls.jsonl"
    ready="$WORK/ready"
    "$PORTAL_BIN" --mode "$MODE" --log "$log" --ready-file "$ready" \
        --accept-path "$WORK" >"$WORK/portal.out" 2>"$WORK/portal.err" &
    portal_pid=$!

    # Wait on the readiness FILE, never on a sleep: a fixed delay is how a slow
    # runner turns "the portal was not up yet" into "the app ignored the portal".
    for _ in $(seq 1 100); do
      [ -f "$ready" ] && break
      kill -0 "$portal_pid" 2>/dev/null || { echo "PORTAL_DIED_EARLY"; cat "$WORK/portal.err"; exit 1; }
      sleep 0.1
    done
    [ -f "$ready" ] || { echo "PORTAL_NEVER_READY"; cat "$WORK/portal.err"; exit 1; }

    probe_rc=0
    if [ -n "$PROBE_ARGS" ]; then
      # shellcheck disable=SC2086
      "$PROBE_BIN" $PROBE_ARGS
      probe_rc=$?
    fi

    kill -INT "$portal_pid" 2>/dev/null
    wait "$portal_pid" 2>/dev/null
    portal_rc=$?

    echo "PROBE_RC=$probe_rc"
    echo "PORTAL_RC=$portal_rc"
    echo "--- recorded calls ---"
    cat "$log" 2>/dev/null || true
  ' >"$work/case.out" 2>&1 || true
  # `|| true` because the inner shell exits 1 on PORTAL_DIED_EARLY /
  # PORTAL_NEVER_READY, and `set -e` would then abort the whole self-test right
  # here: before $W is assigned, before any ok/bad accounting, and before the
  # remaining cases run. It still fails -- the assertions below have nothing to
  # match -- but it fails while saying which case broke and why.
  echo "$work"
}

has() { grep -q -- "$2" "$1/case.out"; }

echo "== 1. cancel: a subscribing client receives Response(1) =="
W=$(run_case cancel "--directory --token tok_cancel --timeout 10")
has "$W" "probe: RESPONSE code=1" \
  && ok "Response(1) reached a client that subscribed the way GTK does" \
  || { bad "no Response(1) received"; sed -n '1,15p' "$W/case.out"; }
has "$W" "MISMATCH" \
  && bad "the returned Request path differs from the predicted one - a real client would miss the reply" \
  || ok "the returned Request path is the one the client predicted"
has "$W" '"directory":true' \
  && ok "recorded with directory=true, the AeroFTP folder-picker shape" \
  || bad "directory=true was not recorded"
has "$W" "PORTAL_RC=0" \
  && ok "portal exits 0 when it was asked" \
  || bad "portal did not exit 0 after a recorded call"

echo "== 2. accept: Response(0) carries a real selection =="
W=$(run_case accept "--token tok_accept --timeout 10")
has "$W" "probe: RESPONSE code=0" \
  && ok "Response(0) received" || bad "no Response(0) received"
has "$W" 'results=\["uris"\]' \
  && ok "the response carries uris, so the success path is not vacuous" \
  || { bad "Response(0) had no uris"; sed -n '1,15p' "$W/case.out"; }

echo "== 3. error: the call itself fails, the way a refusing portal looks =="
W=$(run_case error "--token tok_error --timeout 10")
has "$W" "PROBE_RC=5" \
  && ok "the client saw a D-Bus error rather than a hang" \
  || { bad "the refusing portal did not produce a D-Bus error"; sed -n '1,15p' "$W/case.out"; }
has "$W" '"answered":"dbus-error"' \
  && ok "a refused call is still recorded as 'the portal was asked'" \
  || bad "a refused call was not recorded"

echo "== 4. unused: the portal reports that nobody ever asked it =="
W=$(run_case cancel "")
has "$W" "PORTAL_RC=3" \
  && ok "portal exits 3 when it was never called" \
  || { bad "portal did not exit 3 on an unused run - the whole test could pass vacuously"; sed -n '1,15p' "$W/case.out"; }

echo "== 5. Close() on the handle the caller was given =="
# GTK calls Request.Close() on the RETURNED path when its dialog goes away. The
# stand-in used to export Request on the portal's own path, where that call
# cannot land: every other assertion here still passed, and the only symptom was
# a bus error in the app's log that reads like a portal failure. Pinned by
# pointing the export back at the portal path, which fails this and nothing else.
W=$(run_case cancel "--token tok_close --timeout 10 --close")
has "$W" "probe: CLOSE ok" \
  && ok "Close() reached the Request exported on the returned handle" \
  || { bad "Close() on the returned handle was not dispatched"; sed -n '1,20p' "$W/case.out"; }

echo "== 6. the stand-in answers NetworkMonitor, because owning the name is a promise =="
# Not decoration, and not chooser surface. Measured on a CI runner: with the
# stand-in owning the portal name the app never reached app_ready and the splash
# hit its safety timeout, while the no-portal case in the SAME job started and
# rendered. The difference was one line - Unknown interface
# 'org.freedesktop.portal.NetworkMonitor' - because GTK_USE_PORTAL=1 makes GIO
# route the network monitor through the portal, and WebKit consults it before
# loading any URL. It refused loopback while that URL was serving a 200.
# An incomplete stand-in does not degrade into "no portal": it becomes a portal
# that breaks the app, and the damage shows up nowhere near the file chooser.
W=$(run_case cancel "--netmon")
has "$W" "probe: NETMON available=true" \
  && ok "NetworkMonitor answers, so GIO gets a working network monitor" \
  || { bad "NetworkMonitor is unanswered: the app under test would not load its frontend"; sed -n '1,20p' "$W/case.out"; }
has "$W" 'NETMON GetStatus keys=\["available", "connectivity", "metered"\]' \
  && ok "GetStatus honours the advertised version 3, so a newer GIO is not handed UnknownMethod" \
  || { bad "the stand-in advertises NetworkMonitor v3 without implementing GetStatus"; sed -n '1,20p' "$W/case.out"; }
has "$W" "probe: PROXY lookup=" \
  && ok "ProxyResolver answers, so a URL fetch is not gated on a resolver that cannot reply" \
  || { bad "ProxyResolver is unanswered: WebKit resolves a proxy before every load"; sed -n '1,20p' "$W/case.out"; }

echo
echo "passed: $PASS   failed: $FAIL"
[ "$FAIL" -eq 0 ]
