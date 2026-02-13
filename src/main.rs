use anyhow::{Context, Result};
use clap::Parser;
use std::io::{self, BufWriter, Read, Write};

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

    /// Print timing breakdown to stderr (for profiling)
    #[arg(long = "debug-timing", hide = true)]
    debug_timing: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = jx::filter::parse(&cli.filter)
        .with_context(|| format!("failed to parse filter: {}", cli.filter))?;

    let stdout = io::stdout().lock();
    let mut out = BufWriter::with_capacity(128 * 1024, stdout);

    let config = if cli.raw {
        jx::output::OutputConfig {
            mode: jx::output::OutputMode::Raw,
            indent: String::new(),
        }
    } else if cli.compact {
        jx::output::OutputConfig {
            mode: jx::output::OutputMode::Compact,
            indent: String::new(),
        }
    } else {
        jx::output::OutputConfig {
            mode: jx::output::OutputMode::Pretty,
            indent: if cli.tab {
                "\t".to_string()
            } else {
                " ".repeat(cli.indent as usize)
            },
        }
    };

    // Detect identity-compact passthrough: `. -c` can use simdjson::minify()
    // directly, skipping DOM parse, Value tree, eval, and output serialization.
    let passthrough = if cli.compact {
        jx::filter::passthrough_path(&filter)
    } else {
        None
    };

    let mut had_output = false;

    if cli.null_input {
        let input = jx::value::Value::Null;
        jx::filter::eval::eval(&filter, &input, &mut |v| {
            had_output = true;
            write_value_line(&mut out, &v, &config).ok();
        });
    } else if cli.files.is_empty() {
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read stdin")?;
        if passthrough == Some(jx::filter::PassthroughPath::Identity) {
            let json_len = buf.len();
            let padded = jx::simdjson::pad_buffer(&buf);
            let minified =
                jx::simdjson::minify(&padded, json_len).context("failed to minify JSON")?;
            out.write_all(&minified)?;
            out.write_all(b"\n")?;
            had_output = true;
        } else {
            let text = std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
            process_input(text, &filter, &mut out, &config, &mut had_output)?;
        }
    } else {
        for path in &cli.files {
            if passthrough == Some(jx::filter::PassthroughPath::Identity) {
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
            } else if cli.debug_timing {
                process_padded_timed(path, &filter, &mut out, &config, &mut had_output)?;
            } else {
                let (padded, json_len) = jx::simdjson::read_padded_file(std::path::Path::new(path))
                    .with_context(|| format!("failed to read file: {path}"))?;
                std::str::from_utf8(&padded[..json_len])
                    .with_context(|| format!("file is not valid UTF-8: {path}"))?;
                process_padded(
                    &padded,
                    json_len,
                    &filter,
                    &mut out,
                    &config,
                    &mut had_output,
                )?;
            }
        }
    }

    out.flush()?;

    if cli.exit_status && !had_output {
        std::process::exit(4);
    }

    Ok(())
}

fn process_input(
    text: &str,
    filter: &jx::filter::Filter,
    out: &mut impl Write,
    config: &jx::output::OutputConfig,
    had_output: &mut bool,
) -> Result<()> {
    let padded = jx::simdjson::pad_buffer(text.as_bytes());
    let json_len = text.len();
    process_padded(&padded, json_len, filter, out, config, had_output)
}

fn process_padded(
    padded: &[u8],
    json_len: usize,
    filter: &jx::filter::Filter,
    out: &mut impl Write,
    config: &jx::output::OutputConfig,
    had_output: &mut bool,
) -> Result<()> {
    let input =
        jx::simdjson::dom_parse_to_value(padded, json_len).context("failed to parse JSON")?;

    jx::filter::eval::eval(filter, &input, &mut |v| {
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
    jx::filter::eval::eval(filter, &input, &mut |v| {
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
