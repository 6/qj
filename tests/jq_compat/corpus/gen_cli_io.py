#!/usr/bin/env python3
"""Generate cli_io.toml: jq_diff CLI cases for main.c's and util.c's input,
output and exit-code behavior (format: tests/jq_diff/cli.rs).

  python3 tests/jq_compat/corpus/gen_cli_io.py

The inputs are edge cases of jq's reader: lines around fgets' 4095-byte
chunks, NUL bytes, BOMs, invalid UTF-8, texts spanning files, --seq record
separators, and stream errors. Each is read as a file and on stdin, under
the input modes (-n, -s, -R, --seq, --stream, --stream-errors).
"""

import base64
import os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "cli_io.toml")


def rep(*parts):
    """Content that repeats strings: rep(("a", 3), ("b", 1))."""
    return ("repeat", list(parts))


def raw(b):
    return ("b64", base64.b64encode(b).decode())


# name -> content: a str (UTF-8 text), rep(...), or raw(bytes).
INPUTS = {
    "long4094": rep(('"', 1), ("a", 4092), ('"\n', 1)),
    "long4095": rep(('"', 1), ("a", 4093), ('"\n', 1)),
    "long4096": rep(('"', 1), ("a", 4094), ('"\n', 1)),
    "long8191": rep(('"', 1), ("a", 8189), ('"\n', 1)),
    "long4095-nonl": rep(('"', 1), ("a", 4093), ('"', 1)),
    "long4096-nonl": rep(('"', 1), ("a", 4094), ('"', 1)),
    "straddle": rep(('"', 1), ("a", 4093), ('é€b"\n', 1)),
    "straddle-raw": rep(("a", 4094), ("€\n", 1)),
    "array-4095": rep(("[", 1), ("1,", 2046), ("1]\n", 1)),
    "nul-nonl": raw(b"1 2\x003 4"),
    "nul-nl": raw(b"1 2\x003 4\n5\n"),
    "nul-end": raw(b"[1,2]\x00"),
    "nul-raw": raw(b"a\x00b\nc\x00d"),
    "bom": raw(b'\xef\xbb\xbf{"a":1}\n2\n'),
    "bad-bom": raw(b'\xef\xbb{"a":1}\n'),
    "bad-utf8": raw(b'"\xff\xfe"\n"\xe2\x82"\n'),
    "bad-utf8-raw": raw(b"ab\xffcd\n\xe2\x82\n"),
    "num-eof": "123",
    "num-eof-ws": "123 ",
    "empty": "",
    "ws-only": "  \n\n ",
    "crlf": "a\r\nb\r\n",
    "multi-line-value": '1\n2\n{"a":\n3\n}\n4\n',
    "error-mid-line": "1 2 } 3 4\n5\n",
    "error-then-lines": '[1,\n}\n[2]\n"x\n3\n',
    "unterminated": '{"a":[1,2',
    "seq": raw(b'\x1e1\n\x1e[2]\n\x1e{"a":3}\n'),
    "seq-bad": raw(b'\x1e1\n\x1e[2\n\x1e{"a":3}\n\x1e4'),
    "seq-bad-object": raw(b"\x1e1\n\x1e[2}\n\x1e3\n"),
    "seq-trunc": raw(b"\x1e123"),
    "seq-no-rs": "1 2\n",
    "seq-long": rep(("\x1e[", 1), ("1,", 3000), ('1]\n\x1e"x', 1), ("y", 5000), ("\n\x1e5\n", 1)),
    "stream-bad": '[1,{"a":2},[3]\n',
    "stream-ok": '{"a":[1,{"b":2}],"c":"d"}\n[]\n{}\n3\n',
    "stream-long-error": rep(("[", 1), ("1,", 3000), ("x]\n[5]\n", 1)),
    "deep": rep(("[", 300), ("]", 300), ("\n", 1)),
    "strings": '"a" "b\\u0000c" "d"\n',
}

# Input modes: name -> (flags, programs).
MODES = {
    "c": (["-c"], [".", "[., input_filename, input_line_number]", '[., (try input catch "E")]']),
    "pretty": ([], ["."]),
    "seq": (["-c", "--seq"], [".", "type"]),
    "stream": (["-c", "--stream"], ["."]),
    "stream-errors": (["-c", "--stream-errors"], ["."]),
    "slurp": (["-c", "-s"], [".", "[length, input_line_number]"]),
    "null": (["-c", "-n"], ["[inputs]", '[(try input catch "E"), input_filename, input_line_number]']),
    "raw": (["-c", "-R"], [".", "[length, input_line_number]"]),
    "raw-slurp": (["-c", "-R", "-s"], ["."]),
    "raw-null": (["-c", "-R", "-n"], ["[inputs]"]),
    "seq-stream": (["-c", "--seq", "--stream"], ["."]),
    "seq-slurp": (["-c", "--seq", "-s"], ["."]),
    "stream-slurp": (["-c", "--stream", "-s"], ["."]),
    "stream-null": (["-c", "--stream-errors", "-n"], ["[inputs]"]),
    "raw-seq": (["-c", "-R", "--seq"], ["."]),
}
# Modes also run with the input on stdin.
STDIN_MODES = ["c", "seq", "stream-errors", "slurp", "null", "raw", "raw-slurp"]

# Several files at once.
MULTI_FILES = {
    "a1": "1",
    "a2": "2",
    "a3": "[1,\n2",
    "a4": ",3]\n",
    "lines3": "1\n2\n3\n",
    "bad": "1 }\n2\n",
    "exact4096": rep(("1 ", 2047), ("12", 1)),
    "exact4096-ws": rep(("1 ", 2048),),
    "raw4096": rep(("x", 4095),) ,
    "bad-utf8": raw(b"ab\xffcd\n\xe2\x82\n"),
    "bom": raw(b'\xef\xbb\xbf{"a":1}\n'),
    "prog-bad-utf8.jq": raw(b'"\xff" + (.a | tostring)\n'),
    "prog-nul.jq": raw(b".a\x00 garbage"),
    "prog-error.jq": ".a |\n|\n",
    "obj": '{"a":1,"b":[1,2,{"c":null}],"d":"x\\u00e9"}\n',
    "mods/bad.jq": "def f: .a b;\n",
    "mods/good.jq": "def g: 1;\n",
}

# (name, args, stdin or None, env or None); files are MULTI_FILES.
CASES = [
    ("across-files", ["-c", ".", "a1", "a2"], None, None),
    ("across-files-array", ["-c", ".", "a3", "a4"], None, None),
    ("names-lines", ["-c", "[., input_filename, input_line_number]", "a1", "a2", "lines3"], None, None),
    ("names-lines-2", ["-c", "[., input_filename, input_line_number]", "lines3", "a1", "a2"], None, None),
    ("missing-between", ["-c", ".", "a3", "nonexist", "a4"], None, None),
    ("missing-first", ["-c", ".", "nonexist", "a3", "a4"], None, None),
    ("missing-last", ["-c", ".", "a1", "nonexist"], None, None),
    ("missing-inputs", ["-n", "-c", "[inputs]", "a1", "nonexist", "a2"], None, None),
    ("missing-filename", ["-n", "-c", "input_filename", "nonexist"], None, None),
    ("missing-inputs-filename", ["-n", "-c", "[inputs, input_filename]", "nonexist"], None, None),
    ("missing-unread", ["-n", "-c", "1", "nonexist"], None, None),
    ("missing-exit-status", ["-e", ".", "nonexist"], None, None),
    ("missing-after-output-exit-status", ["-e", ".", "lines3", "nonexist"], None, None),
    ("directory-first", ["-c", ".", ".", "a1"], None, None),
    ("directory-last", ["-c", ".", "a1", "."], None, None),
    ("raw-across-files", ["-R", "-c", ".", "a1", "a2", "a3", "a4"], None, None),
    ("raw-slurp-across-files", ["-Rs", "-c", ".", "a1", "a2", "a3", "a4"], None, None),
    ("slurp-across-files", ["-s", "-c", ".", "a1", "a2", "lines3"], None, None),
    ("parse-error-then-file", ["-c", ".", "bad", "lines3"], None, None),
    ("parse-error-input-then-file", ["-c", "[., (try input catch .)]", "bad", "lines3"], None, None),
    ("stdin-then-file", ["-c", ".", "-", "a1"], "0\n", None),
    ("stdin-twice", ["-c", "[., input_filename]", "-", "-"], "1 2", None),
    ("file-stdin-file", ["-c", ".", "a1", "-", "a2"], "3", None),
    ("bom-second-file", ["-c", ".", "a1", "bom"], None, None),
    ("bom-first-file", ["-c", ".", "bom", "lines3"], None, None),
    # -f
    ("from-file-bad-utf8", ["-f", "prog-bad-utf8.jq", "obj"], None, None),
    ("from-file-nul", ["-f", "prog-nul.jq", "obj"], None, None),
    ("from-file-compile-error", ["-f", "prog-error.jq", "obj"], None, None),
    ("from-file-missing", ["-f", "nonexist.jq", "obj"], None, None),
    ("from-file-directory", ["-f", ".", "obj"], None, None),
    ("from-file-origin", ["-f", "prog-bad-utf8.jq", "-n", "-c", "--arg", "a", "b"], None, None),
    # --slurpfile / --rawfile
    ("slurpfile-4096", ["--slurpfile", "x", "exact4096", "-n", "-c", "$x|length"], None, None),
    ("slurpfile-4096-ws", ["--slurpfile", "x", "exact4096-ws", "-n", "-c", "$x|length"], None, None),
    ("slurpfile-bom", ["--slurpfile", "x", "bom", "-n", "-c", "$x"], None, None),
    ("slurpfile-bad", ["--slurpfile", "x", "bad", "-n", "-c", "$x"], None, None),
    ("slurpfile-missing", ["--slurpfile", "x", "nonexist", "-n", "-c", "$x"], None, None),
    ("slurpfile-directory", ["--slurpfile", "x", ".", "-n", "-c", "$x"], None, None),
    ("rawfile-4095", ["--rawfile", "x", "raw4096", "-n", "-c", "$x|length"], None, None),
    ("rawfile-bad-utf8", ["--rawfile", "x", "bad-utf8", "-n", "-c", "$x"], None, None),
    ("rawfile-bom", ["--rawfile", "x", "bom", "-n", "-c", "$x"], None, None),
    ("rawfile-missing-exit-status", ["-e", "--rawfile", "x", "nonexist", "-n", "$x"], None, None),
    # halt, halt_error, errors and exit codes over three inputs
    ("halt-mid", ["-c", "if . == 2 then halt else . end"], "1 2 3\n", None),
    ("halt-error-null-input-mid", ["-c", "if . == 2 then halt_error else . end"], "1 2 3\n", None),
    ("halt-error-string-mid", ["-c", 'if . == 2 then "x\\n"|halt_error(3) else . end'], "1 2 3\n", None),
    ("halt-error-null-exit-status", ["-e", "-c", "if . == 2 then null|halt_error(0) else . end"], "1 2 3\n", None),
    ("halt-error-object", ["-c", 'if . == 2 then {"a":1}|halt_error(1) else . end'], "1 2 3\n", None),
    ("halt-error-nul-string", ["-c", 'if . == 2 then "a\\u0000b"|halt_error(1) else . end'], "1 2 3\n", None),
    ("error-mid-then-ok", ["-c", 'if . == 2 then error("x") else . end'], "1 2 3\n", None),
    ("error-last", ["-c", 'if . == 3 then error("x") else . end'], "1 2 3\n", None),
    ("exit-status-last-empty", ["-e", "if . == 3 then empty else false end"], "1 2 3\n", None),
    ("exit-status-first-false", ["-e", "if . == 1 then false else empty end"], "1 2 3\n", None),
    ("exit-status-last-error", ["-e", 'if . == 3 then error("x") else true end'], "1 2 3\n", None),
    ("exit-status-parse-error", ["-e", "."], "1 {", None),
    ("exit-status-halt", ["-n", "-e", "halt"], None, None),
    ("exit-status-false-halt", ["-n", "-e", "false, halt"], None, None),
    ("exit-status-halt-error-minus-4", ["-n", "-e", '"x"|halt_error(-4)'], None, None),
    ("exit-status-error-after-false", ["-n", "-e", 'false, error("x")'], None, None),
    ("halt-error-1e10", ["-n", '"x"|halt_error(1e10)'], None, None),
    ("halt-error-minus-3.5", ["-n", '"x"|halt_error(-3.5)'], None, None),
    ("halt-error-minus-3.5-exit-status", ["-n", "-e", '"x"|halt_error(-3.5)'], None, None),
    ("halt-error-256", ["-n", '"x"|halt_error(256)'], None, None),
    ("halt-error-255.9", ["-n", '"x"|halt_error(255.9)'], None, None),
    ("halt-error-nan", ["-n", '"x"|halt_error(nan)'], None, None),
    ("halt-error-int-min", ["-n", "-e", '"x"|halt_error(-2147483648)'], None, None),
    ("halt-error-string-code", ["-n", '"x"|halt_error("a")'], None, None),
    ("halt-error-nul-message", ["-n", '"\\u0000x"|halt_error(5)'], None, None),
    ("error-nul-message", ["-n", '"a\\u0000b" | error'], None, None),
    ("error-object-with-nul", ["-n", '{"a":"b\\u0000"} | error'], None, None),
    ("error-null", ["-n", "1, error(null), 2"], None, None),
    ("error-null-per-input", ["error(null)"], "1 2", None),
    ("error-multiline", ["-n", 'error("x\\ny")'], None, None),
    # output flags
    ("raw-output0-nul", ["-n", "--raw-output0", '"a","b\\u0000c","d"'], None, None),
    ("raw-output0-values", ["-n", "--raw-output0", '1, "a", [2]'], None, None),
    ("raw-output-nul", ["-n", "-r", '"a\\u0000b"'], None, None),
    ("join-output-values", ["-n", "-j", '1, "a", [2], {"a":"b"}'], None, None),
    ("ascii-join-output", ["-n", "-a", "-j", '"é", 1'], None, None),
    ("ascii-raw-output", ["-n", "-a", "-r", '"é\\u0000", ["é"]'], None, None),
    ("seq-raw-output", ["-n", "--seq", "-r", '1, "a"'], None, None),
    ("seq-join-output", ["-n", "--seq", "-j", '1, "a"'], None, None),
    ("color-raw-output", ["-n", "-C", "-r", '"a", {"a":"b"}'], None, None),
    ("color-debug", ["-n", "-C", '{"b":2,"a":"é"} | debug | empty'], None, None),
    ("color-sort-debug", ["-n", "-C", "-S", '{"b":2,"a":"é"} | debug | empty'], None, None),
    ("ascii-debug", ["-n", "-a", '"é" | debug | empty'], None, None),
    ("tab-debug", ["-n", "--tab", "[1] | debug | empty"], None, None),
    ("stderr-string", ["-n", '"a\\u0000b\\n" | stderr | empty'], None, None),
    ("stderr-sorted-unaffected", ["-n", "-S", "-a", '{"b":"é","a":1} | stderr | empty'], None, None),
    ("indent-0-color", ["-n", "-C", "--indent", "0", '{"a":[1,{}]}'], None, None),
    ("tab-color-sort", ["-n", "-C", "-S", "--tab", '{"b":[],"a":{"d":1,"c":2}}'], None, None),
    ("unbuffered", ["-n", "--unbuffered", "-c", "range(3)"], None, None),
    ("disasm", ["--debug-dump-disasm", "-n", ".a | reduce .[] as $x (0; . + $x)"], None, None),
    ("disasm-file", ["--debug-dump-disasm", "-c", ".a", "obj"], None, None),
    ("trace", ["--debug-trace", "-n", "1 + 1"], None, None),
    ("trace-all", ["--debug-trace=all", "-n", "[1,2] | .[0]"], None, None),
    ("trace-error", ["--debug-trace", "-n", '1, error("x")'], None, None),
    ("trace-input", ["--debug-trace", "-c", "[., input]"], "1 2", None),
    # arguments
    ("arg-first-wins", ["--arg", "x", "1", "--arg", "x", "2", "-n", "-c", "$x, $ARGS"], None, None),
    ("arg-named-args", ["--arg", "ARGS", "1", "-n", "-c", "$ARGS"], None, None),
    ("arg-named-env", ["--arg", "ENV", "1", "-n", "-c", "($ENV|type), $ARGS"], None, None),
    ("arg-build-configuration", ["--arg", "JQ_BUILD_CONFIGURATION", "x", "-n", "-c", "$JQ_BUILD_CONFIGURATION"], None, None),
    ("argjson-literals", ["--argjson", "x", "[1.000, 1e2, 100000000000000000001, nan]", "-n", "-c", "$x"], None, None),
    ("args-then-jsonargs", ["-n", "-c", "$ARGS", "--args", "a", "--jsonargs", "1", "--args", "b"], None, None),
    ("rawfile-and-slurpfile-named", ["--rawfile", "r", "a1", "--slurpfile", "s", "lines3", "-n", "-c", "$ARGS"], None, None),
    # A builtin's failed assert() aborts jq: on macOS abort() flushes stdout
    # (the earlier results appear), with glibc it doesn't.
    ("abort-flushes-stdout", ["-n", 'range(3), (1 | _strindices("a"))'], None, None),
    ("abort-flushes-stdout-dates", ["-n", '"x", (1e30 | strflocaltime("%c"))'], None, None),
    ("abort-after-inputs", ["-c", 'if . == 3 then _strindices(1) else . end'], "1 2 3 4", None),
    # modules: errors reported while running (default_err_cb) and compiling
    ("modulemeta-syntax-error", ["-L", "mods", "-n", '"bad" | modulemeta'], None, None),
    ("modulemeta-good", ["-L", "mods", "-n", "-c", '"good" | modulemeta'], None, None),
    ("import-syntax-error", ["-L", "mods", "-n", 'import "bad" as b; 1'], None, None),
    ("include-good", ["-L", "mods", "-n", 'include "good"; g'], None, None),
    # JQ_COLORS
    ("jq-colors-one", ["-n", "-C", "-c", '[null,false,true,1,"s",[1],{"a":1}]'], None, {"JQ_COLORS": "1;31"}),
    ("jq-colors-all", ["-n", "-C", "-c", '[null,false,true,1,"s",[1],{"a":1}]'], None, {"JQ_COLORS": "0;90:0;37:0;37:0;37:0;32:1;37:1;37:34;1"}),
    ("jq-colors-nine", ["-n", "-C", "-c", '[null,false,true,1,"s",[1],{"a":1}]'], None, {"JQ_COLORS": "1:2:3:4:5:6:7:8:9"}),
    ("jq-colors-garbage", ["-n", "-C", "-c", "[null,1]"], None, {"JQ_COLORS": "garbage"}),
    ("jq-colors-trailing-colon", ["-n", "-C", "-c", "[null,1]"], None, {"JQ_COLORS": "1;31:"}),
    ("jq-colors-empty-fields", ["-n", "-C", "-c", "[null,false]"], None, {"JQ_COLORS": "::"}),
    ("jq-colors-no-color", ["-n", "-c", "[null,1]"], None, {"JQ_COLORS": "x"}),
    ("jq-colors-debug", ["-n", "-C", "[null] | debug | empty"], None, {"JQ_COLORS": "1;31"}),
    ("no-color-with-color-flag", ["-n", "-C", "1"], None, {"NO_COLOR": "1"}),
]

# --run-tests (jq_test.c): test files, as files of each case.
RUN_TESTS_FILES = {
    "basic.test": '1+1\nnull\n2\n\n.a\n{"a":3}\n4\n',
    "fail.test": (
        "%%FAIL\n.a b\njq: error: syntax error, unexpected IDENT, expecting end of file "
        "at <top-level>, line 1, column 4:\n    .a b\n       ^\n\n"
        "%%FAIL\n{\njq: error: wrong\n\n"
        "%%FAIL IGNORE MSG\n}\nwhatever\n\n"
        "%%FAIL\n1\njq: error: x\n\n"
        "%%FAIL\n.a b\njq: error: syntax error, unexpected IDENT, expecting end of file "
        "at <top-level>, line 1, column 4:\n    .a b\n       ^\nextra line\n\n"
        "%%FAIL\n.a b\njq: error: syntax error\n\n"
        ".\n1\n1\n"
    ),
    "results.test": (
        ".[]\n[1,2]\n1\n\n.[]\n[1,2]\n1\n2\n3\n\n.\n{\n1\n\n.\n1\n{\n\n$x\nnull\nnull\n\n"
        '[label $f | try break $f catch .]\nnull\n[{"__jq":0}]\n\n'
        '[label $f | try break $f catch .]\nnull\n[{"__jq":1}]\n'
    ),
    "three.test": "1\nnull\n1\n\n2\nnull\n2\n\n3\nnull\n3\n",
    "no-output.test": "1\nnull\n",
    "no-input.test": ".[0]\n",
    "only-fail.test": "%%FAIL\n",
    "builtins.test": (
        'error("x")\nnull\n1\n\ninput\nnull\n1\n\n$__loc__\nnull\n'
        '{"file":"<top-level>","line":1}\n\ndebug\n1\n1\n\n1, halt, 2\nnull\n1\n\n'
        'get_search_list\nnull\n[]\n\n$ENV|type\nnull\n"object"\n'
    ),
    "long-line.test": rep(('"', 1), ("a", 5000), ('"|length\nnull\n5000\n\n1\nnull\n1\n', 1)),
    "jq.test": ("path", "../jq.test"),
    "man.test": ("path", "../man.test"),
    "onig.test": ("path", "../onig.test"),
}

RUN_TESTS_CASES = [
    ("run-tests-basic", ["--run-tests", "basic.test"], None),
    ("run-tests-stdin", ["--run-tests"], RUN_TESTS_FILES["basic.test"]),
    ("run-tests-fail", ["--run-tests", "fail.test"], None),
    ("run-tests-results", ["--run-tests", "results.test"], None),
    ("run-tests-no-output", ["--run-tests", "no-output.test"], None),
    ("run-tests-no-input", ["--run-tests", "no-input.test"], None),
    ("run-tests-only-fail", ["--run-tests", "only-fail.test"], None),
    ("run-tests-builtins", ["--run-tests", "builtins.test"], None),
    ("run-tests-long-line", ["--run-tests", "long-line.test"], None),
    ("run-tests-skip-1", ["--run-tests", "three.test", "--skip", "1"], None),
    ("run-tests-skip-all", ["--run-tests", "three.test", "--skip", "3"], None),
    ("run-tests-skip-past-end", ["--run-tests", "three.test", "--skip", "4"], None),
    ("run-tests-take-1", ["--run-tests", "three.test", "--take", "1"], None),
    ("run-tests-take-0", ["--run-tests", "three.test", "--take", "0"], None),
    ("run-tests-take-before-file", ["--run-tests", "--take", "2", "three.test"], None),
    ("run-tests-skip-take", ["--run-tests", "three.test", "--skip", "1", "--take", "1"], None),
    ("run-tests-missing-file", ["--run-tests", "nonexist.test"], None),
    ("run-tests-last-file-missing", ["--run-tests", "three.test", "nonexist.test"], None),
    ("run-tests-disasm", ["--debug-dump-disasm", "--run-tests", "basic.test"], None),
    ("run-tests-trace", ["--debug-trace", "--run-tests", "basic.test"], None),
    ("run-tests-exit-status-pass", ["-e", "--run-tests", "three.test"], None),
    ("run-tests-exit-status-fail", ["-e", "--run-tests", "basic.test"], None),
    ("run-tests-jq-test", ["-L", "../../modules", "--run-tests", "jq.test"], None),
    ("run-tests-man-test", ["--run-tests", "man.test"], None),
    ("run-tests-onig-test", ["--run-tests", "onig.test"], None),
]

# BOM checks after NUL-led inputs. jq's parser sees none of an input that
# starts with a NUL and has no newline (fgets, then strlen), so a BOM in the
# next input is still at the start of its text.
NUL_BOM_FILES = {
    "nul-only": raw(b"\x00"),
    "nul-lead": raw(b"\x00c\x00"),
    "nul-lead-nl": raw(b"\x00c\n"),
    "bom": raw(b'\xef\xbb\xbf{"a":1}\n'),
    "bad-bom": raw(b"\xef\x01c\x00"),
}

# (name, args, stdin or None); files are NUL_BOM_FILES.
NUL_BOM_CASES = [
    ("nul-file-then-bom", ["-c", ".", "nul-only", "bom"], None),
    ("nul-led-file-then-bom", ["-c", ".", "nul-lead", "bom"], None),
    ("nul-led-file-then-bad-bom", ["-c", ".", "nul-lead", "bad-bom"], None),
    ("nul-led-line-then-bom", ["-c", ".", "nul-lead-nl", "bom"], None),
    ("nul-led-file-then-bom-slurp", ["-s", "-c", ".", "nul-lead", "bom"], None),
    ("nul-led-file-then-bom-inputs", ["-n", "-c", "[inputs]", "nul-lead", "bom"], None),
    ("nul-led-stdin-then-bom", ["-c", ".", "-", "bom"], raw(b"\x00c")),
]


def toml_str(s):
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\t":
            out.append("\\t")
        elif ch == "\r":
            out.append("\\r")
        elif o < 0x20 or o == 0x7F:
            out.append("\\u%04x" % o)
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def content(c):
    if isinstance(c, str):
        return toml_str(c)
    kind, v = c
    if kind == "b64":
        return "{ b64 = %s }" % toml_str(v)
    if kind == "path":
        return "{ path = %s }" % toml_str(v)
    parts = ", ".join("[%s, %d]" % (toml_str(s), n) for s, n in v)
    return "{ repeat = [%s] }" % parts


def arr(xs):
    return "[" + ", ".join(toml_str(x) for x in xs) + "]"


def main():
    lines = [
        "# Generated by gen_cli_io.py; edit that instead. Format: tests/jq_diff/cli.rs.",
        "#",
        "# main.c's and util.c's input handling: fgets chunks, NULs, BOMs, invalid",
        "# UTF-8, texts across files, --seq, --stream(-errors), -n/-s/-R, as files",
        "# and on stdin; several files at once; halt, errors and exit codes; output",
        "# flags; arguments.",
        "",
    ]
    multi = ", ".join('"%s" = %s' % (n, content(c)) for n, c in MULTI_FILES.items())
    for name, args, stdin, env in CASES:
        lines += ["[[case]]", 'name = "%s"' % name, "args = %s" % arr(args)]
        if stdin is not None:
            lines.append("stdin = %s" % content(stdin))
        if any(f in MULTI_FILES or f.startswith("nonexist") or f in (".", "mods") for f in args):
            lines.append("files = { %s }" % multi)
        if env:
            lines.append(
                "env = { %s }" % ", ".join("%s = %s" % (k, toml_str(v)) for k, v in env.items())
            )
        lines.append("")
    tests = ", ".join('"%s" = %s' % (n, content(c)) for n, c in RUN_TESTS_FILES.items())
    for name, args, stdin in RUN_TESTS_CASES:
        lines += ["[[case]]", 'name = "%s"' % name, "args = %s" % arr(args)]
        if stdin is not None:
            lines.append("stdin = %s" % content(stdin))
        lines += ["files = { %s }" % tests, ""]
    nul_bom = ", ".join('"%s" = %s' % (n, content(c)) for n, c in NUL_BOM_FILES.items())
    for name, args, stdin in NUL_BOM_CASES:
        lines += ["[[case]]", 'name = "%s"' % name, "args = %s" % arr(args)]
        if stdin is not None:
            lines.append("stdin = %s" % content(stdin))
        lines += ["files = { %s }" % nul_bom, ""]
    for name, c in INPUTS.items():
        for mode, (flags, programs) in MODES.items():
            lines += [
                "[[sweep]]",
                'name = "file.%s.%s"' % (name, mode),
                "programs = %s" % arr(programs),
                "variants = { f = %s }" % arr(flags + ["{program}", name]),
                'files = { "%s" = %s }' % (name, content(c)),
                "",
            ]
        for mode in STDIN_MODES:
            flags, programs = MODES[mode]
            lines += [
                "[[sweep]]",
                'name = "stdin.%s.%s"' % (name, mode),
                "programs = %s" % arr(programs),
                "variants = { s = %s }" % arr(flags + ["{program}"]),
                "stdin = %s" % content(c),
                "",
            ]
    with open(OUT, "w") as f:
        f.write("\n".join(lines))


main()
