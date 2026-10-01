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
# The BSDs' sh and BusyBox's ash all have `ulimit -s`.
# shellcheck disable=SC3045
test "$(ulimit -s 1024; "$qj" -n '1 + 1')" = 2
test "$(QJ_JQ_COMPAT=1 "$qj" -n '1 + 2')" = 3
echo "smoke checks passed"
