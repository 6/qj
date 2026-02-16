use anyhow::{Context, Result};
use clap::Parser;
use mimalloc::MiMalloc;
use std::io::{self, BufWriter, IsTerminal, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Detect P-core count on Apple Silicon via sysctlbyname(3), fall back to available_parallelism.
/// Only runs on aarch64 macOS — Intel Macs don't have P/E core distinction.
fn default_thread_count() -> usize {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let mut val: i32 = 0;
        let mut size = std::mem::size_of::<i32>();
        let name = b"hw.perflevel0.logicalcpu\0";
        let ret = unsafe {
            libc::sysctlbyname(
                name.as_ptr() as *const libc::c_char,
                &mut val as *mut i32 as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ret == 0 && val > 0 {
            return val as usize;
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[derive(Parser)]
#[command(
    name = "qj",
    about = "qj - a faster jq",
    version,
    after_help = "Example:\n\n\t$ echo '{\"foo\": 0}' | qj .\n\t{\n\t  \"foo\": 0\n\t}"
)]
struct Cli {
    /// jq filter expression (not needed with --from-file/-f)
    filter: Option<String>,

    /// Input file(s); defaults to stdin
    files: Vec<String>,

    /// Compact output (no pretty-printing)
    #[arg(short = 'c', long = "compact-output")]
    compact: bool,

    /// Raw output (strings without quotes)
    #[arg(short = 'r', long = "raw-output")]
    raw: bool,

    /// Raw output with NUL separator instead of newline (implies -r)
    #[arg(long = "raw-output0")]
    raw_output0: bool,

    /// Escape non-ASCII characters to \uXXXX sequences
    #[arg(short = 'a', long = "ascii-output")]
    ascii_output: bool,

    /// Flush stdout after each output value
    #[arg(long = "unbuffered")]
    unbuffered: bool,

    /// Use tab for indentation
    #[arg(long)]
    tab: bool,

    /// Number of spaces for indentation (default: 2)
    #[arg(long, default_value_t = 2)]
    indent: u32,

    /// Set exit status based on output
    #[arg(short = 'e', long = "exit-status")]
    exit_status: bool,

    /// Null input — don't read any input, use `null` as the sole input
    #[arg(short = 'n', long = "null-input")]
    null_input: bool,

    /// Treat input as NDJSON (newline-delimited JSON)
    #[arg(long)]
    jsonl: bool,

    /// Slurp all inputs into an array
    #[arg(short = 's', long = "slurp")]
    slurp: bool,

    /// Read each line as a raw string instead of parsing as JSON
    #[arg(short = 'R', long = "raw-input")]
    raw_input: bool,

    /// Sort object keys
    #[arg(short = 'S', long = "sort-keys")]
    sort_keys: bool,

    /// Don't print newline after each output value
    #[arg(short = 'j', long = "join-output")]
    join_output: bool,

    /// Force color output even when piped
    #[arg(short = 'C', long = "color-output")]
    color: bool,

    /// Monochrome output (no color)
    #[arg(short = 'M', long = "monochrome-output")]
    monochrome: bool,

    /// Bind $name to string value
    #[arg(long = "arg", num_args = 2, value_names = ["NAME", "VALUE"], action = clap::ArgAction::Append)]
    args: Vec<String>,

    /// Bind $name to parsed JSON value
    #[arg(long = "argjson", num_args = 2, value_names = ["NAME", "VALUE"], action = clap::ArgAction::Append)]
    argjson: Vec<String>,

    /// Bind $NAME to raw string contents of FILE
    #[arg(long = "rawfile", num_args = 2, value_names = ["NAME", "FILE"], action = clap::ArgAction::Append)]
    rawfile: Vec<String>,

    /// Bind $NAME to array of JSON values parsed from FILE
    #[arg(long = "slurpfile", num_args = 2, value_names = ["NAME", "FILE"], action = clap::ArgAction::Append)]
    slurpfile: Vec<String>,

    /// Read filter from file instead of first argument
    #[arg(short = 'f', long = "from-file", value_name = "FILE")]
    from_file: Option<String>,

    /// Print timing breakdown to stderr (for profiling)
    #[arg(long = "debug-timing", hide = true)]
    debug_timing: bool,

    /// Number of threads for parallel NDJSON processing
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
}

fn main() -> Result<()> {
    // Restore default SIGPIPE behavior so piping to `head` etc. exits cleanly
    // instead of producing BrokenPipe errors. Rust's runtime sets SIG_IGN by default.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Pre-scan for --args / --jsonargs: split argv before clap sees them.
    // Everything after --args or --jsonargs becomes positional string/JSON values.
    let raw_args: Vec<String> = std::env::args().collect();
    let (clap_args, positional_args, positional_json) = {
        let mut clap_part = raw_args.clone();
        let mut pos_str = Vec::new();
        let mut pos_json = false;
        if let Some(idx) = raw_args
            .iter()
            .position(|a| a == "--args" || a == "--jsonargs")
        {
            pos_json = raw_args[idx] == "--jsonargs";
            let tail: Vec<String> = raw_args[idx + 1..].to_vec();
            clap_part = raw_args[..idx].to_vec();
            pos_str = tail;
        }
        (clap_part, pos_str, pos_json)
    };

    let cli = Cli::parse_from(&clap_args);

    // Configure Rayon thread pool to use P-cores only on Apple Silicon.
    // E-cores add contention without throughput benefit for I/O-bound NDJSON work.
    rayon::ThreadPoolBuilder::new()
        .num_threads(cli.threads.unwrap_or_else(default_thread_count))
        .build_global()
        .ok(); // Ignore error if pool already initialized (e.g., in tests)

    // Resolve filter string and input files.
    // With --from-file, all positional args are input files.
    // Without it, the first positional is the filter expression.
    // If no filter given: default to "." (like jq). On TTY with no files, show usage hint.
    let (filter_str, input_files) = if let Some(ref path) = cli.from_file {
        let filter_str = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read filter file: {path}"))?;
        let mut files = cli.files.clone();
        if let Some(ref f) = cli.filter {
            files.insert(0, f.clone());
        }
        (filter_str, files)
    } else {
        match cli.filter.clone() {
            Some(f) => (f, cli.files.clone()),
            None if !cli.null_input && cli.files.is_empty() && io::stdin().is_terminal() => {
                eprintln!("qj - a faster jq [version {}]", env!("CARGO_PKG_VERSION"));
                eprintln!("Usage: qj [OPTIONS] [FILTER] [FILES...]");
                eprintln!("       echo '{{}}' | qj '.'");
                eprintln!("For help: qj --help");
                std::process::exit(0);
            }
            None => (".".to_string(), cli.files.clone()),
        }
    };

    let filter = match qj::filter::parse(&filter_str) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("qj: error: failed to parse filter: {filter_str}\n\nCaused by:\n    {e}");
            std::process::exit(3);
        }
    };

    // Build environment from --arg / --argjson
    // Variable names in the AST include the '$' prefix (e.g., "$name"),
    // so we prepend '$' when binding.
    let mut env = qj::filter::Env::empty();
    for pair in cli.args.chunks(2) {
        if pair.len() == 2 {
            env = env.bind_var(
                format!("${}", pair[0]),
                qj::value::Value::String(pair[1].clone()),
            );
        }
    }
    for pair in cli.argjson.chunks(2) {
        if pair.len() == 2 {
            let padded = qj::simdjson::pad_buffer(pair[1].as_bytes());
            let val = qj::simdjson::dom_parse_to_value(&padded, pair[1].len())
                .with_context(|| format!("invalid JSON for --argjson {}: {}", pair[0], pair[1]))?;
            env = env.bind_var(format!("${}", pair[0]), val);
        }
    }
    for pair in cli.rawfile.chunks(2) {
        if pair.len() == 2 {
            let content = std::fs::read_to_string(&pair[1])
                .with_context(|| format!("failed to read --rawfile {}: {}", pair[0], pair[1]))?;
            env = env.bind_var(format!("${}", pair[0]), qj::value::Value::String(content));
        }
    }
    for pair in cli.slurpfile.chunks(2) {
        if pair.len() == 2 {
            let buf = std::fs::read(&pair[1])
                .with_context(|| format!("failed to read --slurpfile {}: {}", pair[0], pair[1]))?;
            let mut values = Vec::new();
            qj::input::collect_values_from_buf(&buf, false, &mut values)
                .with_context(|| format!("failed to parse --slurpfile {}: {}", pair[0], pair[1]))?;
            env = env.bind_var(
                format!("${}", pair[0]),
                qj::value::Value::Array(Arc::new(values)),
            );
        }
    }

    // Build $ARGS: {positional: [...], named: {...}}
    {
        let pos_values: Vec<qj::value::Value> = if positional_json {
            let mut vals = Vec::new();
            for s in &positional_args {
                let padded = qj::simdjson::pad_buffer(s.as_bytes());
                match qj::simdjson::dom_parse_to_value(&padded, s.len()) {
                    Ok(v) => vals.push(v),
                    Err(e) => {
                        eprintln!(
                            "qj: invalid JSON text passed to --jsonargs: {s}\n\nCaused by:\n    {e}"
                        );
                        std::process::exit(2);
                    }
                }
            }
            vals
        } else {
            positional_args
                .iter()
                .map(|s| qj::value::Value::String(s.clone()))
                .collect()
        };

        let named_pairs: Vec<(String, qj::value::Value)> = {
            let mut pairs = Vec::new();
            for pair in cli.args.chunks(2) {
                if pair.len() == 2 {
                    pairs.push((pair[0].clone(), qj::value::Value::String(pair[1].clone())));
                }
            }
            for pair in cli.argjson.chunks(2) {
                if pair.len() == 2 {
                    let padded = qj::simdjson::pad_buffer(pair[1].as_bytes());
                    if let Ok(val) = qj::simdjson::dom_parse_to_value(&padded, pair[1].len()) {
                        pairs.push((pair[0].clone(), val));
                    }
                }
            }
            pairs
        };

        let args_obj = qj::value::Value::Object(Arc::new(vec![
            (
                "positional".to_string(),
                qj::value::Value::Array(Arc::new(pos_values)),
            ),
            (
                "named".to_string(),
                qj::value::Value::Object(Arc::new(named_pairs)),
            ),
        ]));
        env = env.bind_var("$ARGS".to_string(), args_obj);
    }

    // Color: on by default for TTY, overridden by -C (force on) or -M (force off).
    // NO_COLOR env var (https://no-color.org/) disables color by default,
    // but -C still overrides it (matches jq behavior).
    // Check before locking stdout.
    let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let use_color = if cli.monochrome {
        false
    } else if cli.color {
        true
    } else if no_color_env {
        false
    } else {
        io::stdout().is_terminal()
    };
    let color_scheme = if use_color {
        qj::output::ColorScheme::jq_default()
    } else {
        qj::output::ColorScheme::none()
    };

    let stdout = io::stdout().lock();
    let mut out = BufWriter::with_capacity(128 * 1024, stdout);

    // -j / --join-output implies raw output (matches jq behavior)
    let config = if cli.raw || cli.raw_output0 || cli.join_output {
        qj::output::OutputConfig {
            mode: qj::output::OutputMode::Raw,
            indent: String::new(),
            sort_keys: cli.sort_keys,
            join_output: cli.join_output,
            color: color_scheme,
            null_separator: cli.raw_output0,
            ascii_output: cli.ascii_output,
            unbuffered: cli.unbuffered,
        }
    } else if cli.compact {
        qj::output::OutputConfig {
            mode: qj::output::OutputMode::Compact,
            indent: String::new(),
            sort_keys: cli.sort_keys,
            join_output: cli.join_output,
            color: color_scheme,
            null_separator: false,
            ascii_output: cli.ascii_output,
            unbuffered: cli.unbuffered,
        }
    } else {
        qj::output::OutputConfig {
            mode: qj::output::OutputMode::Pretty,
            indent: if cli.tab {
                "\t".to_string()
            } else {
                " ".repeat(cli.indent as usize)
            },
            sort_keys: cli.sort_keys,
            join_output: cli.join_output,
            color: color_scheme,
            null_separator: false,
            ascii_output: cli.ascii_output,
            unbuffered: cli.unbuffered,
        }
    };

    // Detect passthrough-eligible patterns. Disable when semantic-changing
    // flags are active (slurp, raw_input, sort_keys, join_output) or when
    // color is enabled (passthrough bypasses the output formatter).
    // Also disable when -e is active — we need full eval to inspect output values.
    let passthrough = if cli.slurp
        || cli.raw_input
        || cli.sort_keys
        || cli.join_output
        || use_color
        || cli.ascii_output
        || cli.raw
        || cli.raw_output0
        || cli.exit_status
    {
        None
    } else {
        qj::filter::passthrough_path(&filter).filter(|p| !p.requires_compact() || cli.compact)
    };

    let uses_input = filter.uses_input_builtins();
    let mut had_output = false;
    let mut had_error = false;
    let mut last_was_falsy = false;

    if cli.null_input {
        // With -n: collect all input values into the input queue (for input/inputs),
        // then eval with null input.
        if uses_input {
            let mut values = Vec::new();
            if !input_files.is_empty() {
                for path in &input_files {
                    if cli.raw_input {
                        let content = std::fs::read_to_string(path)
                            .with_context(|| format!("failed to read file: {path}"))?;
                        for line in content.lines() {
                            values.push(qj::value::Value::String(line.to_string()));
                        }
                    } else {
                        let (padded, json_len) =
                            qj::simdjson::read_padded_file(std::path::Path::new(path))
                                .with_context(|| format!("failed to read file: {path}"))?;
                        qj::input::collect_values_from_buf(
                            &padded[..json_len],
                            cli.jsonl,
                            &mut values,
                        )?;
                    }
                }
            } else {
                let mut buf = Vec::new();
                io::stdin()
                    .read_to_end(&mut buf)
                    .context("failed to read stdin")?;
                if cli.raw_input {
                    let text = std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
                    for line in text.lines() {
                        values.push(qj::value::Value::String(line.to_string()));
                    }
                } else {
                    qj::input::strip_bom(&mut buf);
                    qj::input::collect_values_from_buf(&buf, cli.jsonl, &mut values)?;
                }
            }
            use std::collections::VecDeque;
            qj::filter::eval::set_input_queue(VecDeque::from(values));
        }
        let input = qj::value::Value::Null;
        eval_and_output(
            &filter,
            &input,
            &env,
            &mut out,
            &config,
            &mut had_output,
            &mut had_error,
            &mut last_was_falsy,
        );
    } else if cli.raw_input {
        // --raw-input: read lines as strings instead of parsing JSON
        if input_files.is_empty() {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read stdin")?;
            let text = std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
            process_raw_input(
                text,
                cli.slurp,
                &filter,
                &env,
                &mut out,
                &config,
                &mut had_output,
                &mut had_error,
                &mut last_was_falsy,
            )?;
        } else if cli.slurp {
            // --raw-input --slurp with files: concatenate all file contents
            // into a single string (matches jq -Rs behavior)
            let mut all_text = String::new();
            for path in &input_files {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read file: {path}"))?;
                all_text.push_str(&content);
            }
            let input = qj::value::Value::String(all_text);
            eval_and_output(
                &filter,
                &input,
                &env,
                &mut out,
                &config,
                &mut had_output,
                &mut had_error,
                &mut last_was_falsy,
            );
        } else {
            for path in &input_files {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read file: {path}"))?;
                process_raw_input(
                    &content,
                    false,
                    &filter,
                    &env,
                    &mut out,
                    &config,
                    &mut had_output,
                    &mut had_error,
                    &mut last_was_falsy,
                )?;
            }
        }
    } else if cli.slurp {
        // --slurp: collect all values into an array, eval once
        let mut values = Vec::new();
        if input_files.is_empty() {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read stdin")?;
            qj::input::strip_bom(&mut buf);
            qj::input::collect_values_from_buf(&buf, cli.jsonl, &mut values)?;
        } else {
            for path in &input_files {
                let (padded, json_len) = qj::simdjson::read_padded_file(std::path::Path::new(path))
                    .with_context(|| format!("failed to read file: {path}"))?;
                qj::input::collect_values_from_buf(&padded[..json_len], cli.jsonl, &mut values)?;
            }
        }
        let input = qj::value::Value::Array(Arc::new(values));
        eval_and_output(
            &filter,
            &input,
            &env,
            &mut out,
            &config,
            &mut had_output,
            &mut had_error,
            &mut last_was_falsy,
        );
    } else if input_files.is_empty() {
        // stdin
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read stdin")?;
        qj::input::strip_bom(&mut buf);
        // Empty input produces no output (matches jq behavior)
        let is_empty = buf
            .iter()
            .all(|&b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'));
        if !is_empty {
            if !uses_input
                && !cli.exit_status
                && (cli.jsonl || qj::parallel::ndjson::is_ndjson(&buf))
            {
                let (output, ho) =
                    qj::parallel::ndjson::process_ndjson(&buf, &filter, &config, &env)
                        .context("failed to process NDJSON from stdin")?;
                out.write_all(&output)?;
                had_output |= ho;
            } else if uses_input {
                // Collect all values; first becomes input, rest go to queue
                let mut values = Vec::new();
                qj::input::collect_values_from_buf(&buf, cli.jsonl, &mut values)?;
                let mut queue: std::collections::VecDeque<_> = values.into();
                let input = queue.pop_front().unwrap_or(qj::value::Value::Null);
                qj::filter::eval::set_input_queue(queue);
                eval_and_output(
                    &filter,
                    &input,
                    &env,
                    &mut out,
                    &config,
                    &mut had_output,
                    &mut had_error,
                    &mut last_was_falsy,
                );
            } else {
                let json_len = buf.len();
                let padded = qj::simdjson::pad_buffer(&buf);
                let mut handled = false;
                if let Some(pt) = &passthrough {
                    handled = try_passthrough(&padded, json_len, pt, &mut out, &mut had_output)
                        .context("passthrough failed")?;
                }
                if !handled {
                    process_padded(
                        &padded,
                        json_len,
                        &filter,
                        &env,
                        &mut out,
                        &config,
                        &mut had_output,
                        &mut had_error,
                        &mut last_was_falsy,
                    )?;
                }
            }
        }
    } else {
        // files
        if uses_input {
            // Collect all values from all files; first becomes input, rest go to queue
            let mut values = Vec::new();
            for path in &input_files {
                let (padded, json_len) = qj::simdjson::read_padded_file(std::path::Path::new(path))
                    .with_context(|| format!("failed to read file: {path}"))?;
                qj::input::collect_values_from_buf(&padded[..json_len], cli.jsonl, &mut values)?;
            }
            let mut queue: std::collections::VecDeque<_> = values.into();
            let input = queue.pop_front().unwrap_or(qj::value::Value::Null);
            qj::filter::eval::set_input_queue(queue);
            eval_and_output(
                &filter,
                &input,
                &env,
                &mut out,
                &config,
                &mut had_output,
                &mut had_error,
                &mut last_was_falsy,
            );
        } else {
            let ctx = ProcessCtx {
                passthrough: &passthrough,
                force_jsonl: cli.jsonl,
                filter: &filter,
                env: &env,
                config: &config,
                debug_timing: cli.debug_timing,
            };
            let mut had_file_error = false;
            for path in &input_files {
                match process_file(
                    path,
                    &ctx,
                    &mut out,
                    &mut had_output,
                    &mut had_error,
                    &mut last_was_falsy,
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        // Strip the redundant anyhow context wrapping — just show root cause
                        let root = e.root_cause();
                        eprintln!("qj: error: Could not open file {path}: {root}");
                        had_file_error = true;
                    }
                }
            }
            if had_file_error {
                // Flush buffered output from successfully processed files before exiting
                let _ = out.flush();
                std::process::exit(2);
            }
        }
    }

    out.flush()?;

    if had_error {
        std::process::exit(5);
    }

    if cli.exit_status {
        if !had_output {
            std::process::exit(4);
        }
        if last_was_falsy {
            std::process::exit(1);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Core processing helpers
// ---------------------------------------------------------------------------

/// Evaluate a filter against an input value and write all outputs.
/// After evaluation, checks for uncaught runtime errors and reports them
/// to stderr (like jq's exit-code-5 behavior).
#[allow(clippy::too_many_arguments)]
fn eval_and_output(
    filter: &qj::filter::Filter,
    input: &qj::value::Value,
    env: &qj::filter::Env,
    out: &mut impl Write,
    config: &qj::output::OutputConfig,
    had_output: &mut bool,
    had_error: &mut bool,
    last_was_falsy: &mut bool,
) {
    let mut nul_error = false;
    let mut write_failed = false;
    qj::filter::eval::eval_filter_with_env(filter, input, env, &mut |v| {
        if nul_error || write_failed {
            return;
        }
        // Check for embedded NUL in --raw-output0 mode
        if config.null_separator
            && let qj::value::Value::String(s) = &v
            && s.contains('\0')
        {
            nul_error = true;
            return;
        }
        *last_was_falsy = matches!(v, qj::value::Value::Null | qj::value::Value::Bool(false));
        *had_output = true;
        if qj::output::write_value(out, &v, config).is_err() {
            write_failed = true;
        }
    });
    if nul_error {
        *had_error = true;
        eprintln!("qj: error: Cannot dump a string containing NUL with --raw-output0 option");
    }
    // Check for uncaught runtime errors
    if let Some(err) = qj::filter::eval::take_last_error() {
        *had_error = true;
        let msg = format_error(&err);
        eprintln!("qj: error: {msg}");
    }
}

/// Format an error value for display on stderr.
fn format_error(err: &qj::value::Value) -> String {
    match err {
        qj::value::Value::String(s) => s.clone(),
        other => other.short_desc(),
    }
}

/// Try the passthrough fast path on a padded buffer.
/// Returns `Ok(true)` if handled, `Ok(false)` if the caller should fall back.
fn try_passthrough(
    padded: &[u8],
    json_len: usize,
    passthrough: &qj::filter::PassthroughPath,
    out: &mut impl Write,
    had_output: &mut bool,
) -> Result<bool> {
    match passthrough {
        qj::filter::PassthroughPath::Identity => {
            // Validate that this is a single JSON document before minifying.
            // simdjson's minify doesn't reject multi-doc input (e.g., {"a":1}{"b":2}),
            // so we must verify with a parse first to avoid incorrect passthrough.
            if qj::simdjson::dom_validate(padded, json_len).is_err() {
                return Ok(false);
            }
            let minified = match qj::simdjson::minify(padded, json_len) {
                Ok(m) => m,
                Err(_) => return Ok(false),
            };
            out.write_all(&minified)?;
            out.write_all(b"\n")?;
            *had_output = true;
            Ok(true)
        }
        qj::filter::PassthroughPath::FieldLength(fields) => {
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            match qj::simdjson::dom_field_length(padded, json_len, &field_refs)? {
                Some(result) => {
                    out.write_all(&result)?;
                    out.write_all(b"\n")?;
                    *had_output = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        qj::filter::PassthroughPath::FieldKeys { fields, sorted } => {
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            match qj::simdjson::dom_field_keys(padded, json_len, &field_refs, *sorted)? {
                Some(result) => {
                    out.write_all(&result)?;
                    out.write_all(b"\n")?;
                    *had_output = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        qj::filter::PassthroughPath::FieldType(fields) => {
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            // Get the raw JSON of the target, then check first byte for type
            let raw = if field_refs.is_empty() {
                // Bare `type` — check the input directly
                // Skip leading whitespace
                let first_byte = padded[..json_len]
                    .iter()
                    .find(|&&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
                match first_byte {
                    Some(b'{') => "\"object\"",
                    Some(b'[') => "\"array\"",
                    Some(b'"') => "\"string\"",
                    Some(b't') | Some(b'f') => "\"boolean\"",
                    Some(b'n') => "\"null\"",
                    Some(b'0'..=b'9') | Some(b'-') => "\"number\"",
                    _ => return Ok(false),
                }
            } else {
                let raw = qj::simdjson::dom_find_field_raw(padded, json_len, &field_refs)?;
                let first_byte = raw.first();
                match first_byte {
                    Some(b'{') => "\"object\"",
                    Some(b'[') => "\"array\"",
                    Some(b'"') => "\"string\"",
                    Some(b't') | Some(b'f') => "\"boolean\"",
                    // "null" as raw result means field missing OR actual null value
                    // jq returns "null" for both, so this is correct
                    Some(b'n') => "\"null\"",
                    Some(b'0'..=b'9') | Some(b'-') => "\"number\"",
                    _ => return Ok(false),
                }
            };
            out.write_all(raw.as_bytes())?;
            out.write_all(b"\n")?;
            *had_output = true;
            Ok(true)
        }
        qj::filter::PassthroughPath::FieldHas { fields, key } => {
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            match qj::simdjson::dom_field_has(padded, json_len, &field_refs, key)? {
                Some(result) => {
                    out.write_all(if result { b"true" } else { b"false" })?;
                    out.write_all(b"\n")?;
                    *had_output = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        qj::filter::PassthroughPath::ArrayMapField {
            prefix,
            fields,
            wrap_array,
        } => {
            let prefix_refs: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            match qj::simdjson::dom_array_map_field(
                padded,
                json_len,
                &prefix_refs,
                &field_refs,
                *wrap_array,
            )? {
                Some(result) => {
                    out.write_all(&result)?;
                    out.write_all(b"\n")?;
                    *had_output = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        qj::filter::PassthroughPath::ArrayMapFieldsObj {
            prefix,
            entries,
            wrap_array,
        } => {
            let prefix_refs: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
            let field_refs: Vec<&str> = entries.iter().map(|s| s.as_str()).collect();
            // Pre-encode JSON keys: "fieldname" (with quotes)
            let json_keys: Vec<Vec<u8>> = entries
                .iter()
                .map(|s| {
                    let mut k = Vec::with_capacity(s.len() + 2);
                    k.push(b'"');
                    k.extend_from_slice(s.as_bytes());
                    k.push(b'"');
                    k
                })
                .collect();
            let key_refs: Vec<&[u8]> = json_keys.iter().map(|k| k.as_slice()).collect();
            match qj::simdjson::dom_array_map_fields_obj(
                padded,
                json_len,
                &prefix_refs,
                &key_refs,
                &field_refs,
                *wrap_array,
            )? {
                Some(result) => {
                    out.write_all(&result)?;
                    out.write_all(b"\n")?;
                    *had_output = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        qj::filter::PassthroughPath::ArrayMapBuiltin {
            prefix,
            op,
            wrap_array,
        } => {
            let prefix_refs: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
            let (op_code, sorted, arg) = match op {
                qj::filter::PassthroughBuiltin::Length => (0, true, ""),
                qj::filter::PassthroughBuiltin::Keys => (1, true, ""),
                qj::filter::PassthroughBuiltin::KeysUnsorted => (1, false, ""),
                qj::filter::PassthroughBuiltin::Type => (2, false, ""),
                qj::filter::PassthroughBuiltin::Has(key) => (3, false, key.as_str()),
            };
            match qj::simdjson::dom_array_map_builtin(
                padded,
                json_len,
                &prefix_refs,
                op_code,
                sorted,
                arg,
                *wrap_array,
            )? {
                Some(result) => {
                    out.write_all(&result)?;
                    out.write_all(b"\n")?;
                    *had_output = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
    }
}

/// Bundled processing context to avoid too-many-arguments in process_file.
struct ProcessCtx<'a> {
    passthrough: &'a Option<qj::filter::PassthroughPath>,
    force_jsonl: bool,
    filter: &'a qj::filter::Filter,
    env: &'a qj::filter::Env,
    config: &'a qj::output::OutputConfig,
    debug_timing: bool,
}

/// Process a single file: read, detect NDJSON, try passthrough, or run the
/// normal DOM parse → eval → output pipeline. Optionally prints timing.
fn process_file(
    path: &str,
    ctx: &ProcessCtx,
    out: &mut impl Write,
    had_output: &mut bool,
    had_error: &mut bool,
    last_was_falsy: &mut bool,
) -> Result<()> {
    // Streaming NDJSON path: peek header to detect NDJSON, then stream from
    // the file descriptor in fixed-size windows. Avoids loading the full file
    // into memory — O(window_size) instead of O(file_size).
    // Skip when debug-timing so we get the full pipeline breakdown.
    if !ctx.debug_timing {
        use std::io::Seek;

        let mut file =
            std::fs::File::open(path).with_context(|| format!("failed to open file: {path}"))?;

        // Detect NDJSON by peeking at the start of the file, then stream it
        // in fixed-size windows to keep memory O(window_size) not O(file_size).
        // force_jsonl skips detection (user asserted NDJSON via --jsonl).
        if ctx.force_jsonl
            || qj::parallel::ndjson::detect_ndjson_from_reader(&mut file)
                .with_context(|| format!("failed to read file: {path}"))?
        {
            file.seek(std::io::SeekFrom::Start(0))
                .with_context(|| format!("failed to seek file: {path}"))?;
            let ho = qj::parallel::ndjson::process_ndjson_streaming(
                &mut file, ctx.filter, ctx.config, ctx.env, out,
            )
            .with_context(|| format!("failed to process NDJSON: {path}"))?;
            *had_output |= ho;
            return Ok(());
        }
    }

    // Non-NDJSON: load the full file for single-doc processing
    let t0 = Instant::now();
    let (padded, json_len) = qj::simdjson::read_padded_file(std::path::Path::new(path))
        .with_context(|| format!("failed to read file: {path}"))?;
    let t_read = t0.elapsed();

    // Empty file produces no output (matches jq behavior)
    if json_len == 0 {
        return Ok(());
    }

    // Passthrough fast path
    if let Some(pt) = ctx.passthrough {
        let t1 = Instant::now();
        let handled = try_passthrough(&padded, json_len, pt, out, had_output)
            .with_context(|| format!("passthrough failed: {path}"))?;
        if handled {
            if ctx.debug_timing {
                let t_op = t1.elapsed();
                let total = t_read + t_op;
                let mb = json_len as f64 / (1024.0 * 1024.0);
                let label = match pt {
                    qj::filter::PassthroughPath::Identity => "minify",
                    qj::filter::PassthroughPath::FieldLength(_) => "length",
                    qj::filter::PassthroughPath::FieldKeys { .. } => "keys",
                    qj::filter::PassthroughPath::FieldType(_) => "type",
                    qj::filter::PassthroughPath::FieldHas { .. } => "has",
                    qj::filter::PassthroughPath::ArrayMapField { .. } => "map_field",
                    qj::filter::PassthroughPath::ArrayMapFieldsObj { .. } => "map_fields_obj",
                    qj::filter::PassthroughPath::ArrayMapBuiltin { .. } => "map_builtin",
                };
                eprintln!("--- debug-timing ({label} passthrough): {path} ({mb:.1} MB) ---");
                print_timing_line("read", t_read, total);
                print_timing_line(label, t_op, total);
                print_timing_total(total, mb);
            }
            return Ok(());
        }
        // Passthrough returned None (unsupported type) — fall through to normal pipeline
    }

    // Normal pipeline: DOM parse → eval → output
    std::str::from_utf8(&padded[..json_len])
        .with_context(|| format!("file is not valid UTF-8: {path}"))?;

    if ctx.debug_timing {
        let t1 = Instant::now();
        let input = match qj::simdjson::dom_parse_to_value(&padded, json_len) {
            Ok(v) => v,
            Err(e)
                if e.to_string().contains(&format!(
                    "simdjson error code {}",
                    qj::simdjson::SIMDJSON_CAPACITY
                )) =>
            {
                let text = std::str::from_utf8(&padded[..json_len])
                    .context("file is not valid UTF-8 (serde_json fallback)")?;
                let serde_val: serde_json::Value = serde_json::from_str(text)
                    .context("failed to parse JSON (serde_json fallback for >4GB file)")?;
                qj::value::Value::from(serde_val)
            }
            Err(e) => return Err(e).context("failed to parse JSON"),
        };
        let t_parse = t1.elapsed();

        let t2 = Instant::now();
        let mut values = Vec::new();
        qj::filter::eval::eval_filter(ctx.filter, &input, &mut |v| {
            values.push(v);
        });
        let t_eval = t2.elapsed();

        // Check for uncaught runtime errors from the debug-timing eval path
        if let Some(err) = qj::filter::eval::take_last_error() {
            *had_error = true;
            let msg = format_error(&err);
            eprintln!("qj: error: {msg}");
        }

        let t3 = Instant::now();
        for v in &values {
            *had_output = true;
            if qj::output::write_value(out, v, ctx.config).is_err() {
                break;
            }
        }
        out.flush()?;
        let t_output = t3.elapsed();

        let total = t_read + t_parse + t_eval + t_output;
        let mb = json_len as f64 / (1024.0 * 1024.0);
        eprintln!("--- debug-timing: {path} ({mb:.1} MB) ---");
        print_timing_line("read", t_read, total);
        print_timing_line("parse", t_parse, total);
        print_timing_line("eval", t_eval, total);
        print_timing_line("output", t_output, total);
        print_timing_total(total, mb);
    } else {
        process_padded(
            &padded,
            json_len,
            ctx.filter,
            ctx.env,
            out,
            ctx.config,
            had_output,
            had_error,
            last_was_falsy,
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Input parsing helpers
// ---------------------------------------------------------------------------

/// Process --raw-input text: each line becomes a Value::String.
/// If slurp is true, concatenate all input into a single string (matches jq -Rs).
#[allow(clippy::too_many_arguments)]
fn process_raw_input(
    text: &str,
    slurp: bool,
    filter: &qj::filter::Filter,
    env: &qj::filter::Env,
    out: &mut impl Write,
    config: &qj::output::OutputConfig,
    had_output: &mut bool,
    had_error: &mut bool,
    last_was_falsy: &mut bool,
) -> Result<()> {
    if slurp {
        // jq's -Rs concatenates all input into a single string value (not an array)
        let input = qj::value::Value::String(text.to_string());
        eval_and_output(
            filter,
            &input,
            env,
            out,
            config,
            had_output,
            had_error,
            last_was_falsy,
        );
    } else {
        for line in text.lines() {
            let input = qj::value::Value::String(line.to_string());
            eval_and_output(
                filter,
                &input,
                env,
                out,
                config,
                had_output,
                had_error,
                last_was_falsy,
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_padded(
    padded: &[u8],
    json_len: usize,
    filter: &qj::filter::Filter,
    env: &qj::filter::Env,
    out: &mut impl Write,
    config: &qj::output::OutputConfig,
    had_output: &mut bool,
    had_error: &mut bool,
    last_was_falsy: &mut bool,
) -> Result<()> {
    // Use flat evaluation (lazy, zero-copy) when the filter is safe for it.
    // Flat eval was designed for NDJSON and silently ignores type errors,
    // so we only use it when the filter won't produce errors that need reporting.
    if let Ok(flat_buf) = qj::simdjson::dom_parse_to_flat_buf_tape(padded, json_len) {
        let mut nul_error = false;
        let mut write_failed = false;
        qj::flat_eval::eval_flat(filter, flat_buf.root(), env, &mut |v| {
            if nul_error || write_failed {
                return;
            }
            if config.null_separator
                && let qj::value::Value::String(s) = &v
                && s.contains('\0')
            {
                nul_error = true;
                return;
            }
            *last_was_falsy = matches!(v, qj::value::Value::Null | qj::value::Value::Bool(false));
            *had_output = true;
            if qj::output::write_value(out, &v, config).is_err() {
                write_failed = true;
            }
        });
        if nul_error {
            *had_error = true;
            eprintln!("qj: error: Cannot dump a string containing NUL with --raw-output0 option");
        }
        if let Some(err) = qj::filter::eval::take_last_error() {
            *had_error = true;
            let msg = format_error(&err);
            eprintln!("qj: error: {msg}");
        }
        return Ok(());
    }

    // Regular pipeline: DOM tape walk → flat buffer → Value tree → eval → output
    let input = match qj::simdjson::dom_parse_to_value_fast(padded, json_len) {
        Ok(v) => v,
        Err(e)
            if e.to_string()
                == format!("simdjson error code {}", qj::simdjson::SIMDJSON_CAPACITY) =>
        {
            // simdjson CAPACITY limit (~4GB) — fall back to serde_json
            let text = std::str::from_utf8(&padded[..json_len])
                .context("file is not valid UTF-8 (serde_json fallback)")?;
            let serde_val: serde_json::Value = serde_json::from_str(text)
                .context("failed to parse JSON (serde_json fallback for >4GB file)")?;
            qj::value::Value::from(serde_val)
        }
        Err(e) => {
            // Try multi-doc fallback: serde_json's StreamDeserializer handles
            // concatenated JSON like {"a":1}{"b":2} and whitespace-separated values.
            let text = match std::str::from_utf8(&padded[..json_len]) {
                Ok(t) => t,
                Err(_) => {
                    eprintln!("qj: error (at <stdin>): {e:#}");
                    *had_error = true;
                    return Ok(());
                }
            };
            let mut stream =
                serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
            let mut count = 0usize;
            let mut last_stream_err = None;
            for result in &mut stream {
                match result {
                    Ok(serde_val) => {
                        count += 1;
                        let input = qj::value::Value::from(serde_val);
                        eval_and_output(
                            filter,
                            &input,
                            env,
                            out,
                            config,
                            had_output,
                            had_error,
                            last_was_falsy,
                        );
                    }
                    Err(se) => {
                        last_stream_err = Some(se);
                        break;
                    }
                }
            }
            if count == 0 {
                // Stream produced nothing — report the original simdjson error
                eprintln!("qj: error (at <stdin>): {e:#}");
                *had_error = true;
            } else if let Some(se) = last_stream_err {
                // Partial parse — some docs succeeded, then an error
                eprintln!("qj: error (at <stdin>): {se}");
                *had_error = true;
            }
            return Ok(());
        }
    };
    eval_and_output(
        filter,
        &input,
        env,
        out,
        config,
        had_output,
        had_error,
        last_was_falsy,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Debug timing helpers
// ---------------------------------------------------------------------------

fn print_timing_line(label: &str, dur: Duration, total: Duration) {
    let pct = if total.as_nanos() > 0 {
        dur.as_secs_f64() / total.as_secs_f64() * 100.0
    } else {
        0.0
    };
    eprintln!(
        "  {label:<7} {:>8.2}ms  ({pct:.0}%)",
        dur.as_secs_f64() * 1000.0,
    );
}

fn print_timing_total(total: Duration, mb: f64) {
    eprintln!(
        "  total:  {:>8.2}ms  ({:.0} MB/s)",
        total.as_secs_f64() * 1000.0,
        mb / total.as_secs_f64()
    );
}
