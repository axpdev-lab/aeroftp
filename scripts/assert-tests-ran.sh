#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
#
# Fail a CI lane that selected no tests, or fewer than it promises to run.
#
# `cargo test` with a filter that matches nothing prints "running 0 tests",
# then "test result: ok." and exits 0. A green lane that executed nothing is
# indistinguishable, from outside, from a green lane that proved something.
# The two ways it happens in practice are a rename nobody propagated to the
# workflow, and somebody removing `#[ignore]` from the tests a lane selects
# with `--ignored`, which switches the lane off while looking like an
# improvement. Neither is hypothetical: the password-only fallback lane in
# delta-sync-integration.yml stood up a Docker fixture and ran zero tests
# from 2026-05-16, when its test was renamed, until this script landed.
#
# Usage:
#   assert-tests-ran.sh <log> <count> <what>    at least <count> tests ran
#   assert-tests-ran.sh <log> =<count> <what>   exactly <count> tests ran
#
# `=<count>` is for a step that names ONE test: a second test arriving there
# changes what the step does and should be said out loud. A step that collects
# a whole module or binary takes the plain form, because such a suite is meant
# to grow and an exact match would turn the lane red on the day someone adds a
# legitimate test, which usually gets answered by deleting the check.
#
# The counted number is what libtest REPORTS as passed, summed over every test
# binary in the log, not what the filter looks like it should select. The
# caller pipes cargo through `tee` and must `set -o pipefail` first, or the
# step's status is tee's rather than cargo's, which is the same blindness this
# script exists to remove.
#
# No `set -e` here on purpose: every failure path below ends in an explicit
# exit with a message. `-u` catches a caller that forgot an argument, and
# `-o pipefail` makes the count pipeline honest.
set -uo pipefail

if [ "$#" -ne 3 ]; then
    echo "::error::assert-tests-ran.sh needs <log> <count|=count> <what>, got $# argument(s)"
    exit 2
fi

log=$1
want=$2
what=$3

case "$want" in
    "="*)
        exact=1
        expected=${want#=}
        ;;
    *)
        exact=0
        expected=$want
        ;;
esac

case "$expected" in
    '' | *[!0-9]*)
        echo "::error::assert-tests-ran.sh: '$want' is not a count"
        exit 2
        ;;
esac

if [ ! -s "$log" ]; then
    # An empty or missing log is not "zero tests": it is a run that died before
    # libtest printed anything, and it must not be reported as a count.
    echo "::error::${what}: no test output at ${log}. cargo produced nothing to count,"
    echo "::error::so the step ran no tests and this is not a pass."
    exit 1
fi

# Sum every binary's result line: a step may run more than one. A FAILED line
# does not match, which is correct, because a failing run has already taken the
# step down through pipefail before this script is reached.
ran=$(grep -oE '^test result: ok\. [0-9]+ passed' "$log" \
      | grep -oE '[0-9]+' \
      | awk '{ s += $1 } END { print s + 0 }')

if [ "$exact" -eq 1 ]; then
    if [ "$ran" -ne "$expected" ]; then
        echo "::error::${what}: executed ${ran} test(s), expected exactly ${expected}."
        echo "::error::Either the filter stopped matching, which is a green that proved"
        echo "::error::nothing, or the step now runs more than the one test it names."
        echo "::error::Fix the filter or say the new number here, do not delete this check."
        exit 1
    fi
else
    if [ "$ran" -lt "$expected" ]; then
        echo "::error::${what}: executed ${ran} test(s), expected at least ${expected}."
        echo "::error::A green here with fewer tests means the selection stopped matching,"
        echo "::error::not that the work got simpler. Fix the filter, not this check; and"
        echo "::error::if tests were deliberately removed, lower the number in that commit."
        exit 1
    fi
fi

echo "${what}: ${ran} test(s) executed (expected ${want})."
