//! qj's own help, version and build-configuration text, and jq 1.8.1's.
//!
//! By default these describe qj: `docs/JQ_PORT_PLAN.md` exempts them from
//! comparison with jq. With `QJ_JQ_COMPAT=1` they are jq's, byte for byte:
//! main.c's `usage()`, `jq-1.8.1`, and the `JQ_CONFIG` string of jq's release
//! binary for this platform. Where jq prints them (and with which exit status)
//! is the same in both modes; see [`crate::cli::args`].

use crate::compat::exactly_jq;

const HEADER: &str = "\
Usage:\tqj [options] <jq filter> [file...]
\tqj [options] --args <jq filter> [strings...]
\tqj [options] --jsonargs <jq filter> [JSON_TEXTS...]

qj is a fast, jq-compatible tool for processing JSON inputs, applying the
given filter to its JSON text inputs and producing the filter's results as
JSON on standard output.

The simplest filter is ., which copies qj's input to its output
unmodified except for formatting. For more advanced filters see
the jq(1) manpage (\"man jq\") and/or https://jqlang.org/.

Example:

\t$ echo '{\"foo\": 0}' | qj .
\t{
\t  \"foo\": 0
\t}

";

const OPTIONS: &str = "\
Command options:
  -n, --null-input          use `null` as the single input value;
  -R, --raw-input           read each line as string instead of JSON;
  -s, --slurp               read all inputs into an array and use it as
                            the single input value;
  -c, --compact-output      compact instead of pretty-printed output;
  -r, --raw-output          output strings without escapes and quotes;
      --raw-output0         implies -r and output NUL after each output;
  -j, --join-output         implies -r and output without newline after
                            each output;
  -a, --ascii-output        output strings by only ASCII characters
                            using escape sequences;
  -S, --sort-keys           sort keys of each object on output;
  -C, --color-output        colorize JSON output;
  -M, --monochrome-output   disable colored output;
      --tab                 use tabs for indentation;
      --indent n            use n spaces for indentation (max 7 spaces);
      --unbuffered          flush output stream after each output;
      --stream              parse the input value in streaming fashion;
      --stream-errors       implies --stream and report parse error as
                            an array;
      --seq                 parse input/output as application/json-seq;
  -f, --from-file           load the filter from a file;
  -L, --library-path dir    search modules from the directory;
      --arg name value      set $name to the string value;
      --argjson name value  set $name to the JSON value;
      --slurpfile name file set $name to an array of JSON values read
                            from the file;
      --rawfile name file   set $name to string contents of file;
      --args                consume remaining arguments as positional
                            string values;
      --jsonargs            consume remaining arguments as positional
                            JSON values;
  -e, --exit-status         set exit status code based on the output;
  -V, --version             show the version;
  --build-configuration     show qj's build configuration;
  -h, --help                show the help;
  --                        terminates argument processing;

qj options (not in jq):
      --threads n           number of threads for parallel NDJSON input;
      --jsonl               read input as NDJSON (one JSON text per line);

Input files that don't exist are expanded as glob patterns
(e.g. 'logs/*.json.gz'); .gz and .zst files are decompressed.

Named arguments are also available as $ARGS.named[], while
positional arguments are available as $ARGS.positional[].
";

fn title() -> String {
    format!(
        "qj - a fast, jq-compatible JSON processor [version {}]\n\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// jq 1.8.1's main.c `usage()`, up to where its short and long forms part
/// (`JQ_VERSION` is `1.8.1`).
const JQ_HEADER: &str = "\
jq - commandline JSON processor [version 1.8.1]

Usage:\tjq [options] <jq filter> [file...]
\tjq [options] --args <jq filter> [strings...]
\tjq [options] --jsonargs <jq filter> [JSON_TEXTS...]

jq is a tool for processing JSON inputs, applying the given filter to
its JSON text inputs and producing the filter's results as JSON on
standard output.

The simplest filter is ., which copies jq's input to its output
unmodified except for formatting. For more advanced filters see
the jq(1) manpage (\"man jq\") and/or https://jqlang.org/.

Example:

\t$ echo '{\"foo\": 0}' | jq .
\t{
\t  \"foo\": 0
\t}

";

/// The rest of `usage(2, 1)`, the short form.
const JQ_SHORT_TAIL: &str = "For listing the command options, use jq --help.\n";

/// The rest of `usage(0, 0)`, the long form (`-b` is Windows-only).
const JQ_OPTIONS: &str = "\
Command options:
  -n, --null-input          use `null` as the single input value;
  -R, --raw-input           read each line as string instead of JSON;
  -s, --slurp               read all inputs into an array and use it as
                            the single input value;
  -c, --compact-output      compact instead of pretty-printed output;
  -r, --raw-output          output strings without escapes and quotes;
      --raw-output0         implies -r and output NUL after each output;
  -j, --join-output         implies -r and output without newline after
                            each output;
  -a, --ascii-output        output strings by only ASCII characters
                            using escape sequences;
  -S, --sort-keys           sort keys of each object on output;
  -C, --color-output        colorize JSON output;
  -M, --monochrome-output   disable colored output;
      --tab                 use tabs for indentation;
      --indent n            use n spaces for indentation (max 7 spaces);
      --unbuffered          flush output stream after each output;
      --stream              parse the input value in streaming fashion;
      --stream-errors       implies --stream and report parse error as
                            an array;
      --seq                 parse input/output as application/json-seq;
  -f, --from-file           load the filter from a file;
  -L, --library-path dir    search modules from the directory;
      --arg name value      set $name to the string value;
      --argjson name value  set $name to the JSON value;
      --slurpfile name file set $name to an array of JSON values read
                            from the file;
      --rawfile name file   set $name to string contents of file;
      --args                consume remaining arguments as positional
                            string values;
      --jsonargs            consume remaining arguments as positional
                            JSON values;
  -e, --exit-status         set exit status code based on the output;
  -V, --version             show the version;
  --build-configuration     show jq's build configuration;
  -h, --help                show the help;
  --                        terminates argument processing;

Named arguments are also available as $ARGS.named[], while
positional arguments are available as $ARGS.positional[].
";

/// `JQ_CONFIG` of jq 1.8.1's release binary for this platform: the configure
/// line of the build, which `--build-configuration` prints and
/// `$JQ_BUILD_CONFIGURATION` holds. These are the binaries `mise` installs
/// (`jq-macos-arm64`, `jq-linux-amd64`, ...); a jq built elsewhere has its
/// own.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const JQ_CONFIG: &str = "--host=x86_64-apple-darwin23.6.0 --disable-docs --with-oniguruma=builtin --disable-shared --enable-static --enable-all-static 'CFLAGS=-O2 -pthread -fstack-protector-all' host_alias=x86_64-apple-darwin23.6.0 'CC=clang -target x86_64-apple-darwin23.6.0' LDFLAGS=-dead_strip";
#[cfg(all(target_os = "macos", not(target_arch = "x86_64")))]
const JQ_CONFIG: &str = "--host=arm64-apple-darwin23.6.0 --disable-docs --with-oniguruma=builtin --disable-shared --enable-static --enable-all-static 'CFLAGS=-O2 -pthread -fstack-protector-all' host_alias=arm64-apple-darwin23.6.0 'CC=clang -target arm64-apple-darwin23.6.0' LDFLAGS=-dead_strip";
#[cfg(all(not(target_os = "macos"), target_arch = "aarch64"))]
const JQ_CONFIG: &str = "--host=aarch64-linux-gnu --disable-docs --with-oniguruma=builtin --enable-static --enable-all-static 'CFLAGS=-O2 -pthread -fstack-protector-all' host_alias=aarch64-linux-gnu CC=aarch64-linux-gnu-gcc LDFLAGS=-s CPP=aarch64-linux-gnu-cpp";
#[cfg(all(not(target_os = "macos"), not(target_arch = "aarch64")))]
const JQ_CONFIG: &str = "--host=x86_64-linux-gnu --disable-docs --with-oniguruma=builtin --enable-static --enable-all-static 'CFLAGS=-O2 -pthread -fstack-protector-all' host_alias=x86_64-linux-gnu CC=x86_64-linux-gnu-gcc LDFLAGS=-s CPP=x86_64-linux-gnu-cpp";

/// `-h`/`--help` (jq's `usage(0, 0)`), for stdout.
pub fn help() -> String {
    if exactly_jq() {
        format!("{JQ_HEADER}{JQ_OPTIONS}")
    } else {
        format!("{}{HEADER}{OPTIONS}", title())
    }
}

/// The short usage jq prints on stderr when there's no program
/// (`usage(2, 1)`).
pub fn short_usage() -> String {
    if exactly_jq() {
        format!("{JQ_HEADER}{JQ_SHORT_TAIL}")
    } else {
        format!(
            "{}{HEADER}For listing the command options, use qj --help.\n",
            title()
        )
    }
}

/// `-V`/`--version`: jq's `jq-1.8.1`, or qj's (the format clap printed
/// before, kept for scripts).
pub fn version() -> String {
    if exactly_jq() {
        "jq-1.8.1\n".to_owned()
    } else {
        format!("qj {}\n", env!("CARGO_PKG_VERSION"))
    }
}

/// qj's `--build-configuration`.
const QJ_CONFIG: &str = concat!(
    "qj ",
    env!("CARGO_PKG_VERSION"),
    " (Rust; simdjson parser; parallel NDJSON; gzip and zstd input)"
);

/// `--build-configuration`, also the value of `$JQ_BUILD_CONFIGURATION`.
pub fn build_configuration() -> &'static str {
    if exactly_jq() { JQ_CONFIG } else { QJ_CONFIG }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// jq's text is main.c's, assembled the way `usage()` prints it.
    #[test]
    fn jq_usage_is_main_c_s() {
        let long = format!("{JQ_HEADER}{JQ_OPTIONS}");
        assert!(long.starts_with("jq - commandline JSON processor [version 1.8.1]\n\nUsage:\tjq "));
        assert!(long.ends_with("available as $ARGS.positional[].\n"));
        assert!(!long.contains("qj"));
        assert!(!long.contains("--binary"), "-b is Windows-only in jq");
        let short = format!("{JQ_HEADER}{JQ_SHORT_TAIL}");
        assert!(short.ends_with("\t}\n\nFor listing the command options, use jq --help.\n"));
        assert!(JQ_CONFIG.starts_with("--host="));
    }
}
