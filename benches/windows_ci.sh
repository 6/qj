#!/usr/bin/env bash
# qj vs jq 1.8.1's Windows release binary on Windows, for CI (the Checks
# workflow's windows job). Hosted runners are shared and noisy, so the numbers
# are indicative; the markdown tables go to the job summary when there is one.
#
#   bash benches/windows_ci.sh path/to/qj.exe path/to/jq.exe [records]
# shellcheck disable=SC2016 # jq programs, in single quotes
set -eu
qj=$1
jq=$2
records=${3:-300000}
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
cd "$(mktemp -d)"

# GH Archive-like records, as NDJSON and as one JSON array.
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
ls -l events.ndjson events.json
echo "qj: $("$qj" --version), jq: $("$jq" --version), ${NUMBER_OF_PROCESSORS:-?} logical processors"
{
    echo "### qj vs jq on Windows"
    echo
    echo "$records records: events.ndjson $(wc -c < events.ndjson | tr -d " ") bytes, events.json $(wc -c < events.json | tr -d " ") bytes; ${NUMBER_OF_PROCESSORS:-?} logical processors."
    echo
} >> "$summary"

bench() {
    local name=$1
    shift
    hyperfine -N --warmup 1 --runs 5 --export-markdown "$name.md" \
        --command-name "qj $*" "\"$qj\" $*" \
        --command-name "jq $*" "\"$jq\" $*"
    cat "$name.md" >> "$summary"
    echo >> "$summary"
}

bench identity . events.json
bench length length events.json
bench select -c '"select(.type == \"PushEvent\")"' events.ndjson
bench field -c .actor.login events.ndjson
