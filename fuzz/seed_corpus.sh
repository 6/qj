#!/usr/bin/env bash
# Seeds the fuzz corpora (fuzz/corpus/<target>/, git-ignored) from jq's test
# suites and qj's jq_diff corpus: their programs for fuzz_compile, their
# inputs and data files for the JSON targets. libFuzzer starts from these
# instead of from nothing, and keeps whatever reaches new code.
#
#   bash fuzz/seed_corpus.sh
#   cargo +nightly fuzz run fuzz_compile -s none -- -dict=fuzz/dict/jq.dict -max_total_time=120
set -euo pipefail

cd "$(dirname "$0")/.."
tests=(tests/jq_compat/*.test tests/jq_compat/corpus/*.test)
data=(tests/jq_compat/corpus/data/*)

# Writes line number $1 of every test block (1: the program, 2: its input)
# to $2/seed-<n>. Blocks are separated by blank lines; lines starting with
# `#` are comments, and `%%FAIL` starts a block whose program fails.
lines() {
    mkdir -p "$2"
    awk -v want="$1" -v out="$2" '
        FNR == 1 { line = 0 }
        /^#/ { next }
        /^%%FAIL/ { line = 0; next }
        /^[[:space:]]*$/ { line = 0; next }
        {
            line++
            if (line == want) {
                n++
                f = out "/seed-" n
                printf "%s", $0 > f
                close(f)
            }
        }
    ' "${tests[@]}"
}

# Copies every data file to $1, after the bytes $2 (printf escapes): the
# targets that take options from their first bytes get fixed ones.
files() {
    mkdir -p "$1"
    for f in "${data[@]}"; do
        # shellcheck disable=SC2059 # $2 holds the escapes
        { printf "$2"; cat "$f"; } > "$1/data-$(basename "$f")"
    done
}

lines 1 fuzz/corpus/fuzz_compile
for target in fuzz_parse fuzz_dom; do
    lines 2 "fuzz/corpus/$target"
    files "fuzz/corpus/$target" ''
done
files fuzz/corpus/fuzz_io_reader '\000\125\252'
files fuzz/corpus/fuzz_tape '\000\000'

for target in fuzz_compile fuzz_parse fuzz_dom fuzz_io_reader fuzz_tape; do
    echo "fuzz/corpus/$target: $(find "fuzz/corpus/$target" -type f | wc -l | tr -d ' ') files"
done
