#!/bin/sh
# Drops qj's own build outputs from target/release before CI caches target/,
# which a VM job (FreeBSD, NetBSD) does by hand: they're rebuilt on every run
# anyway, and leaving them would grow the cache without saving any time. The
# dependencies' outputs, which are what the cache is for, stay.
#
#   sh .github/prune_cargo_cache.sh
set -eu
r=target/release
[ -d "$r" ] || exit 0
names="qj"
for t in tests/*.rs; do
    names="$names $(basename "$t" .rs)"
done
for n in $names; do
    rm -rf "$r"/deps/"$n"-* "$r"/deps/lib"$n"-* "$r"/.fingerprint/"$n"-* "$r"/build/"$n"-*
done
rm -rf "$r"/qj "$r"/qj.d "$r"/libqj.* "$r"/incremental target/tmp
du -sh target 2>/dev/null || true
