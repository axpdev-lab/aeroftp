#!/usr/bin/env bash
# #521 pin: when a session fails its precondition, the gate reports one failure
# per scenario and SKIPS chooser assertions instead of cascading eleven reds.
#
# No Tauri build, no X, no D-Bus. Forces portal-chooser-test.sh through
# PORTAL_TEST_FAKE_RC and checks the shape of the summary.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# Retry off: we want a single attempt so the shape is deterministic.
set +e
LOG="$OUT/gate.log"
PORTAL_TEST_FAKE_RC=1 PORTAL_TEST_RETRY=0 PORTAL_TEST_OUT="$OUT/work" \
  "$HERE/portal-chooser-test.sh" /bin/true >"$LOG" 2>&1
rc=$?
set -e

echo "== selftest-precondition (#521) =="
echo "gate exit: $rc"
tail -5 "$LOG" || true

PASS=0
FAIL=0
ok()  { printf '  ok   %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }

# The gate must itself exit non-zero: three scenarios failed their session.
[ "$rc" -ne 0 ] && ok "gate exits non-zero when every session is forced to fail" ||
  bad "gate exited 0 despite forced session failures"

# Exactly three session failures (one per case), never a cascade of chooser reds.
summary=$(grep -E '^passed:|^  FAIL |^  skip |^  ok   ' "$LOG" || true)
fail_n=$(grep -c '^  FAIL ' "$LOG" || true)
skip_n=$(grep -c '^  skip ' "$LOG" || true)
ok_n=$(grep -c '^  ok   ' "$LOG" || true)

[ "$fail_n" = "3" ] &&
  ok "exactly three FAIL lines (one session failure per case, not a cascade)" ||
  bad "expected 3 FAIL lines, got $fail_n"

# Case 1 alone used to emit ~10 chooser failures after a precondition miss.
# With the skip path we expect at least the ten case-1 skips plus case 2 and 3.
[ "$skip_n" -ge 15 ] &&
  ok "at least 15 skip lines for chooser assertions that need a live session (got $skip_n)" ||
  bad "expected >=15 skip lines, got $skip_n"

# No chooser-shaped failure text: those claims are the lie #521 removes.
if grep -E 'FAIL.*(chooser did NOT go out of process|no OpenFile|handle_token|portal was not asked|portal was never asked)' "$LOG" >/dev/null; then
  bad "chooser-shaped FAIL lines still present after a precondition miss"
else
  ok "no chooser-shaped FAIL lines after a precondition miss"
fi

# Every FAIL names the precondition / session, not the chooser.
chooser_fail=0
while IFS= read -r line; do
  case "$line" in
    *'session failed its precondition'*) ;;
    *) chooser_fail=$((chooser_fail + 1)) ;;
  esac
done < <(grep '^  FAIL ' "$LOG" || true)
[ "$chooser_fail" = "0" ] &&
  ok "every FAIL names the session precondition (#521)" ||
  bad "$chooser_fail FAIL line(s) do not name the session precondition"

# Summary line includes skipped.
grep -qE 'passed: .* failed: .* skipped: ' "$LOG" &&
  ok "summary line reports skipped count" ||
  bad "summary line missing skipped count"

echo
echo "selftest passed: $PASS   failed: $FAIL"
echo "(inner gate counts: ok=$ok_n fail=$fail_n skip=$skip_n)"
[ "$FAIL" -eq 0 ]
