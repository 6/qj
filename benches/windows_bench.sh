#!/usr/bin/env bash
# qj vs jq 1.8.1's Windows release binary, for the Benchmarks workflow's
# Windows job. bench_tools (the Linux and macOS jobs') doesn't run on Windows
# yet: it builds hyperfine commands for a POSIX shell. So this times qj and
# jq only, on GH Archive-like records generated with qj, and writes
# results_json_ci.md and results_ndjson_ci.md into OUT_DIR, where the
# workflow's commit-results job picks them up like the other platforms'.
#
#   bash benches/windows_bench.sh path\to\qj.exe path\to\jq.exe OUT_DIR [records]
#
# The tool paths are Windows paths (`cygpath -w`): hyperfine starts them
# without a shell.
# shellcheck disable=SC2016 # jq programs, in single quotes
set -eu
qj=$1
jq=$2
out=$3
records=${4:-300000}
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$(mktemp -d)"

"$qj" -nc -b --argjson n "$records" '
  range($n)
  | {id: tostring,
     type: (["PushEvent", "WatchEvent", "IssuesEvent", "ForkEvent"][. % 4]),
     actor: {id: ., login: "user\(. % 9973)", url: "https://api.github.com/users/user\(. % 9973)"},
     repo: {id: (. % 1000), name: "org\(. % 97)/repo\(. % 1000)"},
     payload: {size: (. % 7), commits: [range(. % 3) as $i | {sha: "\(.)-\($i)", message: "commit \($i) of \(.)"}]},
     public: (. % 5 != 0),
     created_at: "2015-01-01T15:00:00Z"}' > events.ndjson
"$qj" -c -b -s . events.ndjson > events.json

# bench FILE NAME ARGS...: one hyperfine table, appended to FILE.
bench() {
    local file=$1 name=$2
    shift 2
    hyperfine -N --warmup 1 --runs 5 --export-markdown "$name.md" \
        --command-name "qj $*" "\"$qj\" $*" \
        --command-name "jq $*" "\"$jq\" $*"
    { cat "$name.md"; echo; } >> "$file"
}

header() {
    echo "qj vs jq only (bench_tools doesn't run on Windows yet), on $records generated GH Archive-like records: $1 ($(wc -c < "$2" | tr -d ' ') bytes); ${NUMBER_OF_PROCESSORS:-?} logical processors. 5 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine)."
    echo
}

json=$out/results_json_ci.md
header "one JSON array" events.json > "$json"
bench "$json" identity . events.json
bench "$json" length length events.json

ndjson=$out/results_ndjson_ci.md
header NDJSON events.ndjson > "$ndjson"
bench "$ndjson" select -c '"select(.type == \"PushEvent\")"' events.ndjson
bench "$ndjson" field -c .actor.login events.ndjson

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$json" "$ndjson" >> "$GITHUB_STEP_SUMMARY"
fi
