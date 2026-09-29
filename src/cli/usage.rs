//! qj's own help, version and build-configuration text.
//!
//! `docs/JQ_PORT_PLAN.md` exempts these from comparison with jq, so they
//! describe qj. Where jq prints them (and with which exit status) is not
//! exempt; see [`crate::cli::args`].

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

/// `-h`/`--help` (jq's `usage(0, 0)`), for stdout.
pub fn help() -> String {
    format!("{}{HEADER}{OPTIONS}", title())
}

/// The short usage jq prints on stderr when there's no program
/// (`usage(2, 1)`).
pub fn short_usage() -> String {
    format!(
        "{}{HEADER}For listing the command options, use qj --help.\n",
        title()
    )
}

/// `-V`/`--version` (the format clap printed before, kept for scripts).
pub fn version() -> String {
    format!("qj {}\n", env!("CARGO_PKG_VERSION"))
}

/// `--build-configuration`, also the value of `$JQ_BUILD_CONFIGURATION`.
pub const BUILD_CONFIGURATION: &str = concat!(
    "qj ",
    env!("CARGO_PKG_VERSION"),
    " (Rust; simdjson parser; parallel NDJSON; gzip and zstd input)"
);
