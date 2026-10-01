#!/bin/sh
# Smoke checks for a qj built on a platform the Checks workflow doesn't test
# (FreeBSD, NetBSD, musl): a query, and the code that uses the C library's
# platform-dependent bindings: strptime, a stack limit below 8 MB
# (src/cli/stack.rs), and compat mode's reading of it (src/compat.rs).
#
#   sh .github/smoke.sh ./target/release/qj
set -eux
qj=$1
test "$(echo '{"a":1}' | "$qj" .a)" = 1
test "$("$qj" -n '"2015-03-05T23:51:47Z" | strptime("%Y-%m-%dT%H:%M:%SZ") | mktime')" = 1425599507
test "$("$qj" -rn '0 | strftime("%A %B")')" = "Thursday January"
# A stack limit below 8 MB, where qj moves to a stack of its own
# (src/cli/stack.rs). NetBSD's sh can fail to lower its own limit that far
# ("Invalid argument": the kernel's stack randomization can leave more than
# that in use already), which says nothing about qj, so then it's skipped.
# The BSDs' sh and BusyBox's ash all have `ulimit -s`.
status=0
# shellcheck disable=SC3045
out=$(ulimit -s 1024 || exit 99; "$qj" -n '1 + 1') || status=$?
if [ "$status" = 99 ]; then
    echo "skipped the 1 MB stack: the shell can't lower its limit here"
else
    test "$status" = 0
    test "$out" = 2
fi
test "$(QJ_JQ_COMPAT=1 "$qj" -n '1 + 2')" = 3
echo "smoke checks passed"
