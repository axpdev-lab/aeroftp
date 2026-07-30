#!/usr/bin/env bash
# The #464 gate: does AeroFTP's file chooser really leave the process through
# xdg-desktop-portal, and what happens when the portal cancels, refuses, or is
# not there at all.
#
# It drives the REAL app: no test-only feature, no build flag, nothing added to
# the shipped binary. The chooser is reached the way a user reaches it, through
# three named controls, and every claim below is an assertion that fails loudly
# rather than a log line somebody has to read.
#
#   1. portal present, cancels  -> the portal IS called, by GTK, with the app's
#                                  own dialog title, and the app treats the
#                                  cancellation as "no selection" rather than an
#                                  error; the #510 pre-check runs and stays quiet
#   2. portal present, refuses  -> the portal is still called; the app survives
#                                  and does not hang
#   3. portal not installed     -> GTK presents nothing and does NOT fall back,
#                                  and the app TELLS THE USER why instead of
#                                  leaving a button that silently does nothing
#
# #521: when a session fails its precondition (app never reached app_ready,
# window never opened on the private display, etc.), report ONE failure that
# names the session, and SKIP every assertion that needs a live session. A
# cascade of "chooser did NOT go out of process" lines after an environmental
# cold-start is a lie about the product. One optional retry of a failed
# scenario is on by default (PORTAL_TEST_RETRY=1) and is logged when it fires.
#
# Usage: portal-chooser-test.sh /path/to/aeroftp
#
# Pin the cascade-stop without a Tauri build:
#   PORTAL_TEST_FAKE_RC=1 PORTAL_TEST_RETRY=0 \
#     tests/portal-chooser/portal-chooser-test.sh /bin/true
# Expect: three session failures, many skips, zero chooser-shaped fails.
set -uo pipefail

APP="${1:?usage: portal-chooser-test.sh /path/to/aeroftp}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${PORTAL_TEST_OUT:-$(mktemp -d)}"
mkdir -p "$WORK"

# The three controls that lead to a picker, in order. Named, not clicked: a
# coordinate that misses is indistinguishable from a control that did nothing,
# and this test would then pass while proving nothing.
# Overridable so the assertions can be shown to be pins: point it at controls
# that open no chooser and case 1 must go red.
SEQ="${PORTAL_TEST_SEQ:-Export / Import|Import Servers Restore from .aeroftp backup file|Choose .aeroftp file...}"

# One retry of a failed scenario by default: cold-start under Xvfb is
# environmental and has been observed to pass on the same commit after a
# re-run (#521). Set PORTAL_TEST_RETRY=0 to disable (and for the fake pin).
RETRY="${PORTAL_TEST_RETRY:-1}"

PASS=0
FAIL=0
SKIP=0
ok()   { printf '  ok   %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  FAIL %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }
skip() { printf '  skip %s\n' "$1"; SKIP=$((SKIP + 1)); }

# Run one scenario. Echoes the exit code on stdout (the only line callers may
# capture). Retry chatter goes to stderr so it never pollutes `rc=$(...)`.
run_case() {
  local name="$1"; shift
  local out="$WORK/$name"
  local attempt=1
  local max_attempts=1
  local last_rc=1
  if [ "$RETRY" = "1" ]; then
    max_attempts=2
  fi

  while [ "$attempt" -le "$max_attempts" ]; do
    rm -rf "$out"
    mkdir -p "$out"

    if [ -n "${PORTAL_TEST_FAKE_RC:-}" ]; then
      # Harness-only path: force a non-zero session without starting the app,
      # so the cascade-stop (#521) can be pinned on any machine.
      printf 'fake session for %s (PORTAL_TEST_FAKE_RC=%s)\n' \
          "$name" "$PORTAL_TEST_FAKE_RC" >"$WORK/$name.log"
      last_rc="$PORTAL_TEST_FAKE_RC"
    else
      # shellcheck disable=SC2086
      env "$@" RECON_OUT="$out" RECON_PRESS="$SEQ" RECON_POST_SETTLE=6 \
          "$HERE/recon-session.sh" "$APP" >"$WORK/$name.log" 2>&1
      last_rc=$?
    fi

    if [ "$last_rc" -eq 0 ]; then
      if [ "$attempt" -gt 1 ]; then
        printf '  retry of scenario %s succeeded on attempt %s (#521)\n' \
            "$name" "$attempt" >&2
      fi
      echo "$last_rc"
      return
    fi

    # Only retry environmental cold-start (app never reported app_ready). Other
    # harness failures must not be hidden by a blind re-run (#523 shape, #521 pin).
    if [ "$attempt" -lt "$max_attempts" ] &&
        [ -z "${PORTAL_TEST_FAKE_RC:-}" ] &&
        grep -q "::error::the app never reported app_ready" "$WORK/$name.log"; then
      printf '  scenario %s attempt %s missed app_ready (exit %s); retrying once (#521)\n' \
          "$name" "$attempt" "$last_rc" >&2
      mv "$out" "$WORK/$name-attempt1" 2>/dev/null || true
      mv "$WORK/$name.log" "$WORK/$name-attempt1.log" 2>/dev/null || true
      attempt=$((attempt + 1))
      continue
    fi

    echo "$last_rc"
    return
  done
}

# Session-level gate. On success: one ok, return 0. On failure: ONE bad that
# names the precondition, return 1 so the caller skips chooser assertions.
session_ok() {
  local name="$1" rc="$2"
  if [ "$rc" = "0" ]; then
    ok "the session ran to completion"
    return 0
  fi
  bad "the session failed its precondition (exit $rc), see $WORK/${name}.log; chooser assertions for this case are skipped (#521)"
  return 1
}

# `grep -c` prints 0 AND exits 1 when a file has no matching lines, so the
# obvious `grep -c . f 2>/dev/null || echo 0` emits "0\n0" for a file that exists
# and is empty. That two-line value made `[ -ge ]` report "integer expression
# expected" on exactly the failure path this file exists to describe clearly, and
# made the two window counts below compare unequal while both meant zero.
count_lines() { if [ -f "$1" ]; then grep -c . "$1" || true; else echo 0; fi; }
calls() { count_lines "$WORK/$1/portal-calls.jsonl"; }

# "It did not crash" must never pass because there was nothing to read. A missing
# log is a failure, not a clean bill of health -- the first version of this file
# reported three healthy apps from runs that had not produced a log at all.
healthy() {
  local case_name="$1" label="$2" pattern="$3"
  local log="$WORK/$case_name/app.log"
  if [ ! -s "$log" ]; then
    bad "$label: no app log at $log, so nothing was actually checked"
    return
  fi
  if grep -qE "$pattern" "$log"; then
    bad "$label: found /$pattern/ in the app log"
  else
    ok "$label"
  fi
}
call1() { head -1 "$WORK/$1/portal-calls.jsonl" 2>/dev/null; }

echo "== 1. the portal is asked, and the request really comes from GTK =="
rc=$(run_case cancel RECON_MODE=cancel)
if session_ok cancel "$rc"; then
  grep -q "windows with class aeroftp on" "$WORK/cancel.log" &&
    ! grep -q "has NO window on" "$WORK/cancel.log" &&
    ok "the app window is on the private display, not the developer session" ||
    bad "the app did not open on the private display"
  grep -q "ok: the accessibility bus carries only our own" "$WORK/cancel.log" &&
    ok "the accessibility bus is private" || bad "the accessibility bus was not private"
  [ "$(calls cancel)" -ge 1 ] &&
    ok "the chooser reached the portal" ||
    bad "the portal was never asked: the chooser did NOT go out of process"
  call1 cancel | grep -q '"method":"OpenFile"' &&
    ok "it called OpenFile" || bad "no OpenFile call recorded"
  # The token prefix is the evidence that this came through GTK rather than
  # anything the harness could have produced: GTK generates gtk<random>.
  call1 cancel | grep -q '"handle_token":"gtk' &&
    ok "the handle_token is GTK-generated, so the caller really is the toolkit" ||
    bad "the handle_token does not look like GTK: the call may not be the real path"
  call1 cancel | grep -q '"title":"Import Servers"' &&
    ok "the dialog carries the title the app asked for" ||
    bad "the title is not the one the app set"
  call1 cancel | grep -q '"answered":"cancelled"' &&
    ok "the portal answered with a cancellation" || bad "the portal did not answer cancelled"
  healthy cancel "a cancelled chooser leaves the app healthy" "panicked|Splash screen safety timeout"
  # The #510 pre-check must not cry wolf. A warning that fires on healthy hosts is a
  # warning users learn to dismiss, so on a machine that HAS a portal the check has
  # to run and find nothing wrong. Both halves matter: the line proves the frontend
  # asked, and the absence of the other line proves it got the right answer.
  grep -q "chooser checked: presentable" "$WORK/cancel/app.log" 2>/dev/null &&
    ok "the chooser pre-check ran and passed on a host with a portal (#510)" ||
    bad "the pre-check did not run on the happy path: it cannot report anything either"
  grep -q "chooser unavailable" "$WORK/cancel/app.log" 2>/dev/null &&
    bad "a host WITH a portal was warned about a missing chooser (crying wolf)" ||
    ok "no false alarm on a host with a working portal"
else
  skip "the app window is on the private display (needs a live session)"
  skip "the accessibility bus is private (needs a live session)"
  skip "the chooser reached the portal (needs a live session)"
  skip "it called OpenFile (needs a live session)"
  skip "the handle_token is GTK-generated (needs a live session)"
  skip "the dialog carries the title the app asked for (needs a live session)"
  skip "the portal answered with a cancellation (needs a live session)"
  skip "a cancelled chooser leaves the app healthy (needs a live session)"
  skip "the chooser pre-check ran (#510) (needs a live session)"
  skip "no false alarm on a host with a working portal (needs a live session)"
fi

echo "== 2. a portal that refuses does not take the app down =="
rc=$(run_case error RECON_MODE=error)
if session_ok error "$rc"; then
  [ "$(calls error)" -ge 1 ] &&
    ok "the portal was still asked" || bad "the portal was not asked"
  call1 error | grep -q '"answered":"dbus-error"' &&
    ok "the refusal was delivered as a D-Bus error" || bad "the refusal was not recorded"
  healthy error "the app survives a refusing portal" "panicked"
else
  skip "the portal was still asked (needs a live session)"
  skip "the refusal was delivered as a D-Bus error (needs a live session)"
  skip "the app survives a refusing portal (needs a live session)"
fi

echo "== 3. no portal installed: what actually happens =="
rc=$(run_case noportal RECON_NO_PORTAL=1)
if session_ok noportal "$rc"; then
  grep -q "portal service(s) omitted" "$WORK/noportal.log" &&
    ok "the bus could activate everything except the portal" ||
    bad "the no-portal bus was not built as intended"
  # A PIN on measured behaviour: GTK does NOT fall back to the native chooser, so no
  # extra window appears. That stays true after the #510 fix and is still worth
  # holding -- the remedy is a message rendered by the WebView, not a second window,
  # and the day a window does appear here somebody has reintroduced the in-process
  # chooser that corrupts the GLib heap. If this fails, read it before "fixing" it.
  # The reference window count comes from case 1: if that session failed its
  # precondition the file was never written and count_lines would quietly read
  # it as 0, turning a missing baseline into a chooser-shaped FAIL. Missing
  # baseline is a skip, not a verdict (same contract as the #521 cascade-stop).
  if [ ! -f "$WORK/cancel/windows-after.txt" ]; then
    skip "no extra window appears without a portal (case 1 reference missing)"
  else
    wins=$(count_lines "$WORK/noportal/windows-after.txt")
    wins_ref=$(count_lines "$WORK/cancel/windows-after.txt")
    [ "$wins" = "$wins_ref" ] &&
      ok "no extra window appears without a portal (measured: no native fallback)" ||
      bad "window count changed ($wins vs $wins_ref): the fallback behaviour is not what was pinned"
  fi

  # The #510 assertion, and the one that made this case worth revisiting. Until the
  # fix, everything above could pass on an app that presented the user with nothing
  # whatsoever: a button that did nothing, indistinguishable from a cancel, because
  # rfd reports the same `null` for both. "No window appeared" is therefore only
  # half the claim -- the other half is that the app SAID SO.
  #
  # It is asserted through the backend log because the message itself is a toast
  # inside the WebView: no window count, no D-Bus trace and no screenshot diff can
  # see it from out here. `chooser_unavailable` is only ever called by a frontend
  # picker call site, so the line appearing at all is what proves the pre-check is
  # wired into the real click path in the real app.
  grep -q "chooser unavailable, telling the user: portal-missing" "$WORK/noportal/app.log" 2>/dev/null &&
    ok "the app detected the missing portal and reported it to the user (#510)" ||
    bad "no chooser-unavailable report: the picker is silent again, which is the #510 defect"
  # And it must be a real discriminator, not a line the app always prints.
  grep -q "chooser checked: presentable" "$WORK/noportal/app.log" 2>/dev/null &&
    bad "the app called this host's chooser presentable while no portal existed" ||
    ok "the host was not wrongly reported as having a working chooser"
  healthy noportal "the app survives with no portal present" "panicked"
else
  skip "the bus could activate everything except the portal (needs a live session)"
  skip "no extra window appears without a portal (needs a live session)"
  skip "the app detected the missing portal (#510) (needs a live session)"
  skip "the host was not wrongly reported as having a working chooser (needs a live session)"
  skip "the app survives with no portal present (needs a live session)"
fi

echo
echo "passed: $PASS   failed: $FAIL   skipped: $SKIP"
echo "artefacts: $WORK"
[ "$FAIL" -eq 0 ]
