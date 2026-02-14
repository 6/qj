use anyhow::{Context, Result};
use clap::Parser;
use std::io::{self, BufWriter, Read, Write};
use std::rc::Rc;

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
        jx::filter::eval::eval_filter_with_env(&filter, &input, &env, &mut |v| {
            had_output = true;
            write_value_line(&mut out, &v, &config).ok();
        });
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
            jx::filter::eval::eval_filter_with_env(&filter, &input, &env, &mut |v| {
                had_output = true;
                write_value_line(&mut out, &v, &config).ok();
            });
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
        jx::filter::eval::eval_filter_with_env(&filter, &input, &env, &mut |v| {
            had_output = true;
            write_value_line(&mut out, &v, &config).ok();
        });
    } else if cli.files.is_empty() {
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
            match &passthrough {
                Some(jx::filter::PassthroughPath::Identity) => {
                    let json_len = buf.len();
                    let padded = jx::simdjson::pad_buffer(&buf);
                    let minified =
                        jx::simdjson::minify(&padded, json_len).context("failed to minify JSON")?;
                    out.write_all(&minified)?;
                    out.write_all(b"\n")?;
                    had_output = true;
                }
                Some(jx::filter::PassthroughPath::FieldLength(fields)) => {
                    let json_len = buf.len();
                    let padded = jx::simdjson::pad_buffer(&buf);
                    let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
                    match jx::simdjson::dom_field_length(&padded, json_len, &field_refs)
                        .context("failed to compute length")?
                    {
                        Some(result) => {
                            out.write_all(&result)?;
                            out.write_all(b"\n")?;
                            had_output = true;
                        }
                        None => {
                            // Unsupported type — fall back to normal pipeline
                            let text =
                                std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
                            process_input(text, &filter, &env, &mut out, &config, &mut had_output)?;
                        }
                    }
                }
                Some(jx::filter::PassthroughPath::FieldKeys(fields)) => {
                    let json_len = buf.len();
                    let padded = jx::simdjson::pad_buffer(&buf);
                    let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
                    match jx::simdjson::dom_field_keys(&padded, json_len, &field_refs)
                        .context("failed to compute keys")?
                    {
                        Some(result) => {
                            out.write_all(&result)?;
                            out.write_all(b"\n")?;
                            had_output = true;
                        }
                        None => {
                            let text =
                                std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
                            process_input(text, &filter, &env, &mut out, &config, &mut had_output)?;
                        }
                    }
                }
                None => {
                    let text = std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
                    process_input(text, &filter, &env, &mut out, &config, &mut had_output)?;
                }
            }
        }
    } else {
        for path in &cli.files {
            if !cli.debug_timing {
                let (padded, json_len) = jx::simdjson::read_padded_file(std::path::Path::new(path))
                    .with_context(|| format!("failed to read file: {path}"))?;
                if cli.jsonl || jx::parallel::ndjson::is_ndjson(&padded[..json_len]) {
                    let (output, ho) = jx::parallel::ndjson::process_ndjson(
                        &padded[..json_len],
                        &filter,
                        &config,
                        &env,
                    )
                    .with_context(|| format!("failed to process NDJSON: {path}"))?;
                    out.write_all(&output)?;
                    had_output |= ho;
                    continue;
                }
            }
            match &passthrough {
                Some(jx::filter::PassthroughPath::Identity) => {
                    if cli.debug_timing {
                        minify_timed(path, &mut out, &mut had_output)?;
                    } else {
                        let (padded, json_len) =
                            jx::simdjson::read_padded_file(std::path::Path::new(path))
                                .with_context(|| format!("failed to read file: {path}"))?;
                        let minified = jx::simdjson::minify(&padded, json_len)
                            .with_context(|| format!("failed to minify: {path}"))?;
                        out.write_all(&minified)?;
                        out.write_all(b"\n")?;
                        had_output = true;
                    }
                }
                Some(jx::filter::PassthroughPath::FieldLength(fields)) => {
                    if cli.debug_timing {
                        field_length_timed(path, fields, &mut out, &mut had_output)?;
                    } else {
                        let (padded, json_len) =
                            jx::simdjson::read_padded_file(std::path::Path::new(path))
                                .with_context(|| format!("failed to read file: {path}"))?;
                        let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
                        match jx::simdjson::dom_field_length(&padded, json_len, &field_refs)
                            .with_context(|| format!("failed to compute length: {path}"))?
                        {
                            Some(result) => {
                                out.write_all(&result)?;
                                out.write_all(b"\n")?;
                                had_output = true;
                            }
                            None => {
                                std::str::from_utf8(&padded[..json_len])
                                    .with_context(|| format!("file is not valid UTF-8: {path}"))?;
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
                    }
                }
                Some(jx::filter::PassthroughPath::FieldKeys(fields)) => {
                    let (padded, json_len) =
                        jx::simdjson::read_padded_file(std::path::Path::new(path))
                            .with_context(|| format!("failed to read file: {path}"))?;
                    let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
                    match jx::simdjson::dom_field_keys(&padded, json_len, &field_refs)
                        .with_context(|| format!("failed to compute keys: {path}"))?
                    {
                        Some(result) => {
                            out.write_all(&result)?;
                            out.write_all(b"\n")?;
                            had_output = true;
                        }
                        None => {
                            std::str::from_utf8(&padded[..json_len])
                                .with_context(|| format!("file is not valid UTF-8: {path}"))?;
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
                }
                None => {
                    if cli.debug_timing {
                        process_padded_timed(path, &filter, &mut out, &config, &mut had_output)?;
                    } else {
                        let (padded, json_len) =
                            jx::simdjson::read_padded_file(std::path::Path::new(path))
                                .with_context(|| format!("failed to read file: {path}"))?;
                        std::str::from_utf8(&padded[..json_len])
                            .with_context(|| format!("file is not valid UTF-8: {path}"))?;
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
            }
        }
    }

    out.flush()?;

    if cli.exit_status && !had_output {
        std::process::exit(4);
    }

    Ok(())
}

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
        jx::filter::eval::eval_filter_with_env(filter, &input, env, &mut |v| {
            *had_output = true;
            write_value_line(out, &v, config).ok();
        });
    } else {
        for line in text.lines() {
            let input = jx::value::Value::String(line.to_string());
            jx::filter::eval::eval_filter_with_env(filter, &input, env, &mut |v| {
                *had_output = true;
                write_value_line(out, &v, config).ok();
            });
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

fn process_input(
    text: &str,
    filter: &jx::filter::Filter,
    env: &jx::filter::Env,
    out: &mut impl Write,
    config: &jx::output::OutputConfig,
    had_output: &mut bool,
) -> Result<()> {
    let padded = jx::simdjson::pad_buffer(text.as_bytes());
    let json_len = text.len();
    process_padded(&padded, json_len, filter, env, out, config, had_output)
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

    jx::filter::eval::eval_filter_with_env(filter, &input, env, &mut |v| {
        *had_output = true;
        write_value_line(out, &v, config).ok();
    });

    Ok(())
}

fn minify_timed(path: &str, out: &mut impl Write, had_output: &mut bool) -> Result<()> {
    use std::time::Instant;

    let t0 = Instant::now();
    let (padded, json_len) = jx::simdjson::read_padded_file(std::path::Path::new(path))
        .with_context(|| format!("failed to read file: {path}"))?;
    let t_read = t0.elapsed();

    let t1 = Instant::now();
    let minified = jx::simdjson::minify(&padded, json_len)
        .with_context(|| format!("failed to minify: {path}"))?;
    let t_minify = t1.elapsed();

    let t2 = Instant::now();
    out.write_all(&minified)?;
    out.write_all(b"\n")?;
    out.flush()?;
    *had_output = true;
    let t_write = t2.elapsed();

    let total = t_read + t_minify + t_write;
    let mb = json_len as f64 / (1024.0 * 1024.0);
    eprintln!("--- debug-timing (minify passthrough): {path} ({mb:.1} MB) ---");
    eprintln!(
        "  read:   {:>8.2}ms  ({:.0}%)",
        t_read.as_secs_f64() * 1000.0,
        t_read.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  minify: {:>8.2}ms  ({:.0}%)  [simdjson::minify]",
        t_minify.as_secs_f64() * 1000.0,
        t_minify.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  write:  {:>8.2}ms  ({:.0}%)",
        t_write.as_secs_f64() * 1000.0,
        t_write.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  total:  {:>8.2}ms  ({:.0} MB/s)",
        total.as_secs_f64() * 1000.0,
        mb / total.as_secs_f64()
    );

    Ok(())
}

fn field_length_timed(
    path: &str,
    fields: &[String],
    out: &mut impl Write,
    had_output: &mut bool,
) -> Result<()> {
    use std::time::Instant;

    let t0 = Instant::now();
    let (padded, json_len) = jx::simdjson::read_padded_file(std::path::Path::new(path))
        .with_context(|| format!("failed to read file: {path}"))?;
    let t_read = t0.elapsed();

    let t1 = Instant::now();
    let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
    let result = jx::simdjson::dom_field_length(&padded, json_len, &field_refs)
        .with_context(|| format!("failed to compute length: {path}"))?;
    let t_length = t1.elapsed();

    let t2 = Instant::now();
    if let Some(data) = result {
        out.write_all(&data)?;
        out.write_all(b"\n")?;
        *had_output = true;
    }
    out.flush()?;
    let t_write = t2.elapsed();

    let total = t_read + t_length + t_write;
    let mb = json_len as f64 / (1024.0 * 1024.0);
    let field_path = if fields.is_empty() {
        ".".to_string()
    } else {
        format!(".{}", fields.join("."))
    };
    eprintln!(
        "--- debug-timing (field length passthrough {field_path} | length): {path} ({mb:.1} MB) ---"
    );
    eprintln!(
        "  read:   {:>8.2}ms  ({:.0}%)",
        t_read.as_secs_f64() * 1000.0,
        t_read.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  length: {:>8.2}ms  ({:.0}%)  [DOM parse + navigate + length]",
        t_length.as_secs_f64() * 1000.0,
        t_length.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  write:  {:>8.2}ms  ({:.0}%)",
        t_write.as_secs_f64() * 1000.0,
        t_write.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  total:  {:>8.2}ms  ({:.0} MB/s)",
        total.as_secs_f64() * 1000.0,
        mb / total.as_secs_f64()
    );

    Ok(())
}

fn process_padded_timed(
    path: &str,
    filter: &jx::filter::Filter,
    out: &mut impl Write,
    config: &jx::output::OutputConfig,
    had_output: &mut bool,
) -> Result<()> {
    use std::time::Instant;

    let t0 = Instant::now();
    let (padded, json_len) = jx::simdjson::read_padded_file(std::path::Path::new(path))
        .with_context(|| format!("failed to read file: {path}"))?;
    std::str::from_utf8(&padded[..json_len])
        .with_context(|| format!("file is not valid UTF-8: {path}"))?;
    let t_read = t0.elapsed();

    let t1 = Instant::now();
    let input =
        jx::simdjson::dom_parse_to_value(&padded, json_len).context("failed to parse JSON")?;
    let t_parse = t1.elapsed();

    let t2 = Instant::now();
    let mut values = Vec::new();
    jx::filter::eval::eval_filter(filter, &input, &mut |v| {
        values.push(v);
    });
    let t_eval = t2.elapsed();

    let t3 = Instant::now();
    for v in &values {
        *had_output = true;
        write_value_line(out, v, config).ok();
    }
    out.flush()?;
    let t_output = t3.elapsed();

    let total = t_read + t_parse + t_eval + t_output;
    let mb = json_len as f64 / (1024.0 * 1024.0);
    eprintln!("--- debug-timing: {path} ({mb:.1} MB) ---");
    eprintln!(
        "  read:   {:>8.2}ms  ({:.0}%)",
        t_read.as_secs_f64() * 1000.0,
        t_read.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  parse:  {:>8.2}ms  ({:.0}%)  [DOM→flat + flat→Value]",
        t_parse.as_secs_f64() * 1000.0,
        t_parse.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  eval:   {:>8.2}ms  ({:.0}%)",
        t_eval.as_secs_f64() * 1000.0,
        t_eval.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  output: {:>8.2}ms  ({:.0}%)",
        t_output.as_secs_f64() * 1000.0,
        t_output.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    eprintln!(
        "  total:  {:>8.2}ms  ({:.0} MB/s)",
        total.as_secs_f64() * 1000.0,
        mb / total.as_secs_f64()
    );

    Ok(())
}

fn write_value_line(
    out: &mut impl Write,
    value: &jx::value::Value,
    config: &jx::output::OutputConfig,
) -> io::Result<()> {
    jx::output::write_value(out, value, config)
}
