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

    let mut had_output = false;

    if cli.null_input {
        let input = jx::value::Value::Null;
        jx::filter::eval::eval(&filter, &input, &mut |v| {
            had_output = true;
            write_value_line(&mut out, &v, &config).ok();
        });
    } else if cli.files.is_empty() {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .context("failed to read stdin")?;
        process_input(&buf, &filter, &mut out, &config, &mut had_output)?;
    } else {
        for path in &cli.files {
            let buf =
                std::fs::read(path).with_context(|| format!("failed to read file: {path}"))?;
            let text = std::str::from_utf8(&buf)
                .with_context(|| format!("file is not valid UTF-8: {path}"))?;
            process_input(text, &filter, &mut out, &config, &mut had_output)?;
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
    let input =
        jx::simdjson::dom_parse_to_value(&padded, json_len).context("failed to parse JSON")?;

    jx::filter::eval::eval(filter, &input, &mut |v| {
        *had_output = true;
        write_value_line(out, &v, config).ok();
    });

    Ok(())
}

fn write_value_line(
    out: &mut impl Write,
    value: &jx::value::Value,
    config: &jx::output::OutputConfig,
) -> io::Result<()> {
    jx::output::write_value(out, value, config)
}
