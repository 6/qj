#!/usr/bin/env bash
# Smoke checks for qj on Windows, where the conformance suites don't run yet:
# each case runs qj and jq 1.8.1's Windows release binary with the same
# arguments and stdin, and compares stdout, stderr (with `qj:` read as `jq:`)
# and the exit status byte for byte. The cases lean on what Windows does
# differently: text-mode stdio (CRLF out, CRLF and Ctrl-Z in) and `-b`, input
# files, `-f`/`--rawfile`/`--slurpfile`, exit codes, the C runtime's time
# functions and error messages, and jq's own strptime.
#
#   bash .github/windows_smoke.sh path/to/qj.exe path/to/jq.exe
# shellcheck disable=SC2016 # jq programs, in single quotes
set -u
qj=$1
jq=$2
# Git Bash rewrites arguments that look like Unix paths; jq programs aren't.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
cd "$(mktemp -d)" || exit 1

: > empty
printf '{"a":1}' > a.json
printf '{"a":1,"b":[1,2]}\r\n{"a":2}\r\n' > crlf.json
printf 'line1\r\nline2\r\n' > crlf.txt
printf '.a\r\n' > prog.jq
printf '1\n\x1a2\n' > ctrlz.json
printf '{"a":' > truncated.json
mkdir dir
"$qj" -nc -b 'range(20000) | {n: ., s: "x\(.)"}' > records.ndjson

# GNU sed on Windows reads files in text mode, dropping the \r of \r\n,
# unless it's given -b; other seds (macOS's) have no -b.
sed=(sed)
if sed -b '' < /dev/null > /dev/null 2>&1; then
    sed=(sed -b)
fi

fails=0
show() {
    echo "    $1:"
    od -c "$2" | head -8 | sed 's/^/      /'
}

# check NAME STDIN ARGS...: qj and jq agree.
check() {
    local name=$1 stdin=$2
    shift 2
    "$qj" "$@" < "$stdin" > q.out 2> q.err
    local qs=$?
    "$jq" "$@" < "$stdin" > j.out 2> j.err
    local js=$?
    "${sed[@]}" 's/^qj:/jq:/' q.err > q.err.jq
    if cmp -s q.out j.out && cmp -s q.err.jq j.err && [ "$qs" = "$js" ]; then
        echo "ok   $name"
    else
        echo "FAIL $name (exit qj $qs, jq $js)"
        show "qj stdout" q.out
        show "jq stdout" j.out
        show "qj stderr" q.err.jq
        show "jq stderr" j.err
        fails=$((fails + 1))
    fi
}

# Text-mode stdio and input files.
check stdin a.json .a
check pretty-file empty . crlf.json
check compact-file empty -c . crlf.json
check raw-file empty -R . crlf.txt
check raw-stdin crlf.txt -R .
check binary-stdin crlf.txt -b -R .
check binary-out a.json -b .
check raw-out empty -nr '"a\nb"'
check join-out empty -nj '"a\nb"'
check ctrl-z-file empty . ctrlz.json
check ctrl-z-stdin ctrlz.json .
check slurp crlf.json -c -s .
check stream crlf.json -c --stream .
check seq a.json --seq .
check tab crlf.json --tab .
check color a.json -C -c .
check filename empty -c input_filename crlf.json
check records empty -c 'select(.n % 3 == 0) | .s' records.ndjson
# Files jq reads itself.
check program-file a.json -f prog.jq
check rawfile empty -n --rawfile f crlf.txt '$f'
check slurpfile empty -nc --slurpfile f crlf.json '$f'
# Errors and exit codes.
check missing-file empty . nope.json
check directory empty . dir
check parse-error truncated.json .
check error empty -n 'error("x")'
check halt-error empty -n '"bye\n" | halt_error(300)'
check exit-status a.json -e .b
# Time: the C runtime's functions, and jq's strptime.
check strptime empty -nc '"2015-03-05T23:51:47Z" | strptime("%Y-%m-%dT%H:%M:%SZ")'
check mktime empty -n '"2015-03-05T23:51:47Z" | strptime("%Y-%m-%dT%H:%M:%SZ") | mktime'
check todate empty -n '1425599507 | todate'
check before-1970 empty -n '-1 | todate'
check strftime-fields empty -n '[2015,2,5,23,51,47,0,0] | strftime("%A %j")'
check strftime-invalid empty -n '0 | strftime("%k")'
check gmtime-type empty -n '"x" | gmtime'
check localtime empty -nc '0 | localtime'
check now empty -n 'now | type'
# The rest of the language, on Windows.
check args empty -nc '$ARGS' --args a 'b c'
check unicode-arg empty -n --arg x 'héllo ✓' '$x'
check regex empty -nc '"foo bar" | [match("o+"; "g").string]'
check numbers empty -nc '[1e1000, 0.1 + 0.2, 3.0, -0, 100000000000000000000]'
check math empty -nc '[(1 | exp), (2 | sqrt), (10 | log), (0.5 | sin), pow(2; 0.5)]'
check bessel empty -nc '[(1 | j0), (1 | j1), (2 | y0), (2 | y1), jn(2; 1.5), yn(2; 1.5)]'
check libm-missing empty -n '4 | significand'
check formats empty -nr '[1, "a b"] | @sh, @csv, @base64'

printf '%.0s[' $(seq 3000) > deep.json
printf '%.0s]' $(seq 3000) >> deep.json
check deep-nesting empty -c . deep.json

# qj only: values nested 100,000 deep, compared recursively, don't run out of
# stack (jq's 2 MB stack would).
deep='def deep: reduce range(100000) as $_ (null; [.]); deep as $a | deep as $b | $a == $b'
if [ "$("$qj" -n "$deep" | tr -d '\r')" = true ]; then
    echo "ok   deep-values"
else
    echo "FAIL deep-values"
    fails=$((fails + 1))
fi
# qj only: compat mode models jq's Unix builds, so it refuses to run here.
if ! QJ_JQ_COMPAT=1 "$qj" -n 1 > q.out 2> q.err && grep -q 'not supported on Windows' q.err; then
    echo "ok   compat-mode"
else
    echo "FAIL compat-mode"
    show "qj stderr" q.err
    fails=$((fails + 1))
fi

echo "$fails failed"
[ "$fails" = 0 ]
