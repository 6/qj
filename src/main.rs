use anyhow::{Context, Result};
use clap::Parser;
use std::io::{self, BufWriter, Read, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "jx", about = "A faster jq", version)]
struct Cli {
    /// jq filter expression
    filter: String,

    /// Input file(s); defaults to stdin
    files: Vec<String>,

    /// Compact output (no pretty-printing)
    #[arg(short = 'c', long = "compact-output")]
    compact: bool,

    /// Raw output (strings without quotes)
    #[arg(short = 'r', long = "raw-output")]
    raw: bool,

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

    /// Monochrome output (no-op, jx doesn't emit color yet)
    #[arg(short = 'M', long = "monochrome-output")]
    #[allow(dead_code)]
    monochrome: bool,

    /// Bind $name to string value
    #[arg(long = "arg", num_args = 2, value_names = ["NAME", "VALUE"], action = clap::ArgAction::Append)]
    args: Vec<String>,

    /// Bind $name to parsed JSON value
    #[arg(long = "argjson", num_args = 2, value_names = ["NAME", "VALUE"], action = clap::ArgAction::Append)]
    argjson: Vec<String>,

    /// Print timing breakdown to stderr (for profiling)
    #[arg(long = "debug-timing", hide = true)]
    debug_timing: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = jx::filter::parse(&cli.filter)
        .with_context(|| format!("failed to parse filter: {}", cli.filter))?;

    // Build environment from --arg / --argjson
    // Variable names in the AST include the '$' prefix (e.g., "$name"),
    // so we prepend '$' when binding.
    let mut env = jx::filter::Env::empty();
    for pair in cli.args.chunks(2) {
        if pair.len() == 2 {
            env = env.bind_var(
                format!("${}", pair[0]),
                jx::value::Value::String(pair[1].clone()),
            );
        }
    }
    for pair in cli.argjson.chunks(2) {
        if pair.len() == 2 {
            let padded = jx::simdjson::pad_buffer(pair[1].as_bytes());
            let val = jx::simdjson::dom_parse_to_value(&padded, pair[1].len())
                .with_context(|| format!("invalid JSON for --argjson {}: {}", pair[0], pair[1]))?;
            env = env.bind_var(format!("${}", pair[0]), val);
        }
    }

    let stdout = io::stdout().lock();
    let mut out = BufWriter::with_capacity(128 * 1024, stdout);

    let config = if cli.raw {
        jx::output::OutputConfig {
            mode: jx::output::OutputMode::Raw,
            indent: String::new(),
            sort_keys: cli.sort_keys,
            join_output: cli.join_output,
        }
    } else if cli.compact {
        jx::output::OutputConfig {
            mode: jx::output::OutputMode::Compact,
            indent: String::new(),
            sort_keys: cli.sort_keys,
            join_output: cli.join_output,
        }
    } else {
        jx::output::OutputConfig {
            mode: jx::output::OutputMode::Pretty,
            indent: if cli.tab {
                "\t".to_string()
            } else {
                " ".repeat(cli.indent as usize)
            },
            sort_keys: cli.sort_keys,
            join_output: cli.join_output,
        }
    };

    // Detect passthrough-eligible patterns. Disable when semantic-changing
    // flags are active (slurp, raw_input, sort_keys, join_output).
    let passthrough = if cli.slurp || cli.raw_input || cli.sort_keys || cli.join_output {
        None
    } else {
        jx::filter::passthrough_path(&filter).filter(|p| !p.requires_compact() || cli.compact)
    };

    let mut had_output = false;

    if cli.null_input {
        let input = jx::value::Value::Null;
        eval_and_output(&filter, &input, &env, &mut out, &config, &mut had_output);
    } else if cli.raw_input {
        // --raw-input: read lines as strings instead of parsing JSON
        if cli.files.is_empty() {
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
            )?;
        } else if cli.slurp {
            // --raw-input --slurp with files: collect all lines from all files
            let mut all_lines = Vec::new();
            for path in &cli.files {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read file: {path}"))?;
                for line in content.lines() {
                    all_lines.push(jx::value::Value::String(line.to_string()));
                }
            }
            let input = jx::value::Value::Array(Rc::new(all_lines));
            eval_and_output(&filter, &input, &env, &mut out, &config, &mut had_output);
        } else {
            for path in &cli.files {
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
                )?;
            }
        }
    } else if cli.slurp {
        // --slurp: collect all values into an array, eval once
        let mut values = Vec::new();
        if cli.files.is_empty() {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read stdin")?;
            collect_values_from_buf(&buf, cli.jsonl, &mut values)?;
        } else {
            for path in &cli.files {
                let (padded, json_len) = jx::simdjson::read_padded_file(std::path::Path::new(path))
                    .with_context(|| format!("failed to read file: {path}"))?;
                collect_values_from_buf(&padded[..json_len], cli.jsonl, &mut values)?;
            }
        }
        let input = jx::value::Value::Array(Rc::new(values));
        eval_and_output(&filter, &input, &env, &mut out, &config, &mut had_output);
    } else if cli.files.is_empty() {
        // stdin
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read stdin")?;
        if cli.jsonl || jx::parallel::ndjson::is_ndjson(&buf) {
            let (output, ho) = jx::parallel::ndjson::process_ndjson(&buf, &filter, &config, &env)
                .context("failed to process NDJSON from stdin")?;
            out.write_all(&output)?;
            had_output |= ho;
        } else {
            let json_len = buf.len();
            let padded = jx::simdjson::pad_buffer(&buf);
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
                )?;
            }
        }
    } else {
        // files
        let ctx = ProcessCtx {
            passthrough: &passthrough,
            force_jsonl: cli.jsonl,
            filter: &filter,
            env: &env,
            config: &config,
            debug_timing: cli.debug_timing,
        };
        for path in &cli.files {
            process_file(path, &ctx, &mut out, &mut had_output)?;
        }
    }

    out.flush()?;

    if cli.exit_status && !had_output {
        std::process::exit(4);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Core processing helpers
// ---------------------------------------------------------------------------

/// Evaluate a filter against an input value and write all outputs.
fn eval_and_output(
    filter: &jx::filter::Filter,
    input: &jx::value::Value,
    env: &jx::filter::Env,
    out: &mut impl Write,
    config: &jx::output::OutputConfig,
    had_output: &mut bool,
) {
    jx::filter::eval::eval_filter_with_env(filter, input, env, &mut |v| {
        *had_output = true;
        jx::output::write_value(out, &v, config).ok();
    });
}

/// Try the passthrough fast path on a padded buffer.
/// Returns `Ok(true)` if handled, `Ok(false)` if the caller should fall back.
fn try_passthrough(
    padded: &[u8],
    json_len: usize,
    passthrough: &jx::filter::PassthroughPath,
    out: &mut impl Write,
    had_output: &mut bool,
) -> Result<bool> {
    match passthrough {
        jx::filter::PassthroughPath::Identity => {
            let minified = jx::simdjson::minify(padded, json_len)?;
            out.write_all(&minified)?;
            out.write_all(b"\n")?;
            *had_output = true;
            Ok(true)
        }
        jx::filter::PassthroughPath::FieldLength(fields) => {
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            match jx::simdjson::dom_field_length(padded, json_len, &field_refs)? {
                Some(result) => {
                    out.write_all(&result)?;
                    out.write_all(b"\n")?;
                    *had_output = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        jx::filter::PassthroughPath::FieldKeys(fields) => {
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            match jx::simdjson::dom_field_keys(padded, json_len, &field_refs)? {
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
    passthrough: &'a Option<jx::filter::PassthroughPath>,
    force_jsonl: bool,
    filter: &'a jx::filter::Filter,
    env: &'a jx::filter::Env,
    config: &'a jx::output::OutputConfig,
    debug_timing: bool,
}

/// Process a single file: read, detect NDJSON, try passthrough, or run the
/// normal DOM parse → eval → output pipeline. Optionally prints timing.
fn process_file(
    path: &str,
    ctx: &ProcessCtx,
    out: &mut impl Write,
    had_output: &mut bool,
) -> Result<()> {
    let t0 = Instant::now();
    let (padded, json_len) = jx::simdjson::read_padded_file(std::path::Path::new(path))
        .with_context(|| format!("failed to read file: {path}"))?;
    let t_read = t0.elapsed();

    // NDJSON fast path (skip when debug-timing so we get the full pipeline breakdown)
    if !ctx.debug_timing
        && (ctx.force_jsonl || jx::parallel::ndjson::is_ndjson(&padded[..json_len]))
    {
        let (output, ho) = jx::parallel::ndjson::process_ndjson(
            &padded[..json_len],
            ctx.filter,
            ctx.config,
            ctx.env,
        )
        .with_context(|| format!("failed to process NDJSON: {path}"))?;
        out.write_all(&output)?;
        *had_output |= ho;
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
                    jx::filter::PassthroughPath::Identity => "minify",
                    jx::filter::PassthroughPath::FieldLength(_) => "length",
                    jx::filter::PassthroughPath::FieldKeys(_) => "keys",
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
        let input =
            jx::simdjson::dom_parse_to_value(&padded, json_len).context("failed to parse JSON")?;
        let t_parse = t1.elapsed();

        let t2 = Instant::now();
        let mut values = Vec::new();
        jx::filter::eval::eval_filter(ctx.filter, &input, &mut |v| {
            values.push(v);
        });
        let t_eval = t2.elapsed();

        let t3 = Instant::now();
        for v in &values {
            *had_output = true;
            jx::output::write_value(out, v, ctx.config).ok();
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
            &padded, json_len, ctx.filter, ctx.env, out, ctx.config, had_output,
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Input parsing helpers
// ---------------------------------------------------------------------------

/// Process --raw-input text: each line becomes a Value::String.
/// If slurp is true, collect all lines into an array.
fn process_raw_input(
    text: &str,
    slurp: bool,
    filter: &jx::filter::Filter,
    env: &jx::filter::Env,
    out: &mut impl Write,
    config: &jx::output::OutputConfig,
    had_output: &mut bool,
) -> Result<()> {
    if slurp {
        let arr: Vec<jx::value::Value> = text
            .lines()
            .map(|l| jx::value::Value::String(l.to_string()))
            .collect();
        let input = jx::value::Value::Array(Rc::new(arr));
        eval_and_output(filter, &input, env, out, config, had_output);
    } else {
        for line in text.lines() {
            let input = jx::value::Value::String(line.to_string());
            eval_and_output(filter, &input, env, out, config, had_output);
        }
    }
    Ok(())
}

/// Collect parsed JSON values from a buffer (single doc or NDJSON lines).
/// Tries single-doc parse first; if that fails and the buffer has newlines,
/// falls back to line-by-line parsing (handles `1\n2\n3` style multi-value input).
fn collect_values_from_buf(
    buf: &[u8],
    force_jsonl: bool,
    values: &mut Vec<jx::value::Value>,
) -> Result<()> {
    if force_jsonl || jx::parallel::ndjson::is_ndjson(buf) {
        parse_lines(buf, values)?;
    } else {
        let json_len = buf.len();
        let padded = jx::simdjson::pad_buffer(buf);
        match jx::simdjson::dom_parse_to_value(&padded, json_len) {
            Ok(val) => values.push(val),
            Err(_) if memchr::memchr(b'\n', buf).is_some() => {
                // Single-doc parse failed but buffer has newlines — try line-by-line
                parse_lines(buf, values)?;
            }
            Err(e) => return Err(e).context("failed to parse JSON"),
        }
    }
    Ok(())
}

fn parse_lines(buf: &[u8], values: &mut Vec<jx::value::Value>) -> Result<()> {
    for line in buf.split(|&b| b == b'\n') {
        let trimmed_end = line
            .iter()
            .rposition(|&b| !matches!(b, b' ' | b'\t' | b'\r'))
            .map_or(0, |p| p + 1);
        let trimmed = &line[..trimmed_end];
        if trimmed.is_empty() {
            continue;
        }
        let padded = jx::simdjson::pad_buffer(trimmed);
        let val = jx::simdjson::dom_parse_to_value(&padded, trimmed.len())
            .context("failed to parse NDJSON line")?;
        values.push(val);
    }
    Ok(())
}

fn process_padded(
    padded: &[u8],
    json_len: usize,
    filter: &jx::filter::Filter,
    env: &jx::filter::Env,
    out: &mut impl Write,
    config: &jx::output::OutputConfig,
    had_output: &mut bool,
) -> Result<()> {
    let input =
        jx::simdjson::dom_parse_to_value(padded, json_len).context("failed to parse JSON")?;
    eval_and_output(filter, &input, env, out, config, had_output);
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
