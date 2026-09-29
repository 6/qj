use anyhow::{Context, Result};
use mimalloc::MiMalloc;
use qj::cli::args::{self, Action, ArgError, ArgValue, ProgramArgument, print_flags};
use std::io::{self, BufWriter, IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// File I/O helpers (compression-aware; `-` is stdin, as in jq)
// ---------------------------------------------------------------------------

/// Read all of stdin.
fn read_stdin() -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    io::stdin()
        .read_to_end(&mut buf)
        .context("failed to read stdin")?;
    Ok(buf)
}

/// Read an input file's bytes, decompressing if needed.
fn read_input_bytes(path: &str) -> Result<Vec<u8>> {
    if path == "-" {
        read_stdin()
    } else if qj::decompress::is_compressed(path) {
        qj::decompress::decompress_file(path)
    } else {
        std::fs::read(path).with_context(|| format!("failed to read file: {path}"))
    }
}

/// Collect parsed JSON values from a file, decompressing if needed.
/// Preserves mmap for uncompressed files via `read_padded_file`.
fn collect_file_values(
    path: &str,
    force_jsonl: bool,
    values: &mut Vec<qj::value::Value>,
) -> Result<()> {
    if path == "-" {
        let mut buf = read_stdin()?;
        qj::input::strip_bom(&mut buf);
        qj::input::collect_values_from_buf(&buf, force_jsonl, values)
    } else if qj::decompress::is_compressed(path) {
        let bytes = qj::decompress::decompress_file(path)?;
        qj::input::collect_values_from_buf(&bytes, force_jsonl, values)
    } else {
        let (padded, json_len) = qj::simdjson::read_padded_file(std::path::Path::new(path))
            .with_context(|| format!("failed to read file: {path}"))?;
        qj::input::collect_values_from_buf(&padded[..json_len], force_jsonl, values)
    }
}

/// Read a file as a UTF-8 string, decompressing if needed.
fn read_file_text(path: &str) -> Result<String> {
    if path == "-" {
        String::from_utf8(read_stdin()?).context("stdin is not valid UTF-8")
    } else if qj::decompress::is_compressed(path) {
        let bytes = qj::decompress::decompress_file(path)?;
        String::from_utf8(bytes).with_context(|| format!("file is not valid UTF-8: {path}"))
    } else {
        std::fs::read_to_string(path).with_context(|| format!("failed to read file: {path}"))
    }
}

/// Extract RS-delimited (RFC 7464) JSON values from a buffer.
/// Each segment after an RS byte (0x1E) up to the next RS or end of buffer
/// is parsed as a JSON value. Segments that fail to parse are silently skipped
/// (with a warning to stderr, matching jq behavior). Content before the first
/// RS byte is also silently skipped.
fn collect_seq_values(buf: &[u8], values: &mut Vec<qj::value::Value>) -> Result<()> {
    let segments: Vec<&[u8]> = buf.split(|&b| b == 0x1E).collect();
    // The first segment (before any RS) is skipped — jq ignores non-RS-prefixed content
    for seg in segments.iter().skip(1) {
        let trimmed: &[u8] =
            seg.iter()
                .position(|b| !b.is_ascii_whitespace())
                .map_or(&[], |start| {
                    let end = seg
                        .iter()
                        .rposition(|b| !b.is_ascii_whitespace())
                        .unwrap_or(start);
                    &seg[start..=end]
                });
        if trimmed.is_empty() {
            continue;
        }
        let padded = qj::simdjson::pad_buffer(trimmed);
        match qj::simdjson::dom_parse_to_value(&padded, trimmed.len()) {
            Ok(v) => values.push(v),
            Err(e) => {
                eprintln!("qj: ignoring parse error: {e}");
            }
        }
    }
    Ok(())
}

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Count non-efficiency cores on Apple Silicon via sysctlbyname(3), fall back to
/// available_parallelism. Only runs on aarch64 macOS — Intel Macs don't have core tiers.
fn default_thread_count() -> usize {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        if let Some(n) = apple_non_efficiency_cpus() {
            return n;
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Sum logical CPUs across every perflevel not named "Efficiency". M1–M4 expose
/// "Performance" + "Efficiency"; M5 Pro/Max expose "Super" + "Performance" with no
/// efficiency tier, so counting only perflevel0 would leave two thirds of the cores idle.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn apple_non_efficiency_cpus() -> Option<usize> {
    fn sysctl_raw(name: &str, buf: &mut [u8]) -> Option<usize> {
        let name = std::ffi::CString::new(name).ok()?;
        let mut size = buf.len();
        let ret = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (ret == 0).then_some(size)
    }
    fn sysctl_i32(name: &str) -> Option<i32> {
        let mut buf = [0u8; 4];
        (sysctl_raw(name, &mut buf)? == 4).then(|| i32::from_ne_bytes(buf))
    }
    fn sysctl_string(name: &str) -> Option<String> {
        let mut buf = [0u8; 64];
        let len = sysctl_raw(name, &mut buf)?;
        let bytes = &buf[..len];
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(len);
        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    let levels = sysctl_i32("hw.nperflevels")?;
    let mut total = 0usize;
    for i in 0..levels {
        let cpus = sysctl_i32(&format!("hw.perflevel{i}.logicalcpu"))?;
        if sysctl_string(&format!("hw.perflevel{i}.name"))? != "Efficiency" && cpus > 0 {
            total += cpus as usize;
        }
    }
    (total > 0).then_some(total)
}

/// The old core's side of jq's option loop: JSON for `--argjson`,
/// `--jsonargs` and `--slurpfile`.
struct ArgJson;

impl args::ArgHost for ArgJson {
    type Value = qj::value::Value;

    fn parse_json(&mut self, text: &[u8]) -> std::result::Result<Self::Value, String> {
        let mut values = Vec::new();
        qj::input::collect_values_from_buf(text, false, &mut values)
            .map_err(|e| format!("{e:#}"))?;
        match values.len() {
            1 => Ok(values.pop().unwrap()),
            0 => Err("Expected JSON value".to_string()),
            _ => Err("Unexpected extra JSON values".to_string()),
        }
    }

    fn slurp_json(&mut self, data: &[u8]) -> std::result::Result<Self::Value, String> {
        let mut values = Vec::new();
        qj::input::collect_values_from_buf(data, false, &mut values)
            .map_err(|e| format!("{e:#}"))?;
        Ok(qj::value::Value::Array(Arc::new(values)))
    }
}

/// A named or positional argument as an old-core value. jq makes strings with
/// `jv_string`, which replaces invalid UTF-8 as well (not always with the
/// same number of U+FFFD).
fn arg_value(v: &ArgValue<qj::value::Value>) -> qj::value::Value {
    match v {
        ArgValue::Text(bytes) => qj::value::Value::String(String::from_utf8_lossy(bytes).into()),
        ArgValue::Json(value) => value.clone(),
    }
}

/// Print qj's own help or version text and exit as jq does: 0, or 2 when
/// stdout can't be written.
fn print_and_exit(text: &str) -> ! {
    let mut stdout = io::stdout().lock();
    let ok = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush());
    std::process::exit(if ok.is_ok() { 0 } else { 2 });
}

/// util.c's message for an input file that can't be processed: "Could not
/// open file FILE: REASON" (with the C library's `strerror`), except that a
/// directory opens fine in jq and then fails to read, which it reports as just
/// the reason.
fn file_error_message(path: &str, e: &anyhow::Error) -> String {
    let root = e.root_cause();
    match root
        .downcast_ref::<io::Error>()
        .and_then(io::Error::raw_os_error)
    {
        Some(libc::EISDIR) => String::from_utf8_lossy(&args::strerror(libc::EISDIR)).into(),
        Some(errno) => format!(
            "Could not open file {path}: {}",
            String::from_utf8_lossy(&args::strerror(errno))
        ),
        None => format!("Could not open file {path}: {root}"),
    }
}

/// Report a command-line error exactly as jq does, and exit.
fn fail(err: &ArgError) -> ! {
    let _ = io::stderr().write_all(&err.render("qj"));
    std::process::exit(err.exit_code());
}

/// Check if a filter AST contains import/include/module statements.
fn has_module_stmts(filter: &qj::filter::Filter) -> bool {
    matches!(
        filter,
        qj::filter::Filter::Import { .. }
            | qj::filter::Filter::Include { .. }
            | qj::filter::Filter::ModuleDecl { .. }
    )
}

fn main() -> Result<()> {
    // Restore default SIGPIPE behavior so piping to `head` etc. exits cleanly
    // instead of producing BrokenPipe errors. Rust's runtime sets SIG_IGN by default.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // jq's option loop (src/cli/args.rs), in the locale jq would use.
    let argv = args::argv_bytes();
    let opts = match args::with_environment_locale(|| args::parse(&argv, &mut ArgJson)) {
        Ok(Action::Run(opts)) => opts,
        Ok(Action::Help) => print_and_exit(&qj::cli::usage::help()),
        Ok(Action::Version) => print_and_exit(&qj::cli::usage::version()),
        Ok(Action::BuildConfiguration) => {
            print_and_exit(&format!("{}\n", qj::cli::usage::BUILD_CONFIGURATION))
        }
        Ok(Action::RunTests { .. }) => {
            eprintln!("qj: error: --run-tests is not supported");
            std::process::exit(2);
        }
        Err(e) => fail(&e),
    };

    // Configure Rayon thread pool to skip efficiency cores on Apple Silicon.
    // E-cores add contention without throughput benefit for I/O-bound NDJSON work.
    rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.unwrap_or_else(default_thread_count))
        .build_global()
        .ok(); // Ignore error if pool already initialized (e.g., in tests)

    // The rest of main.c's setup, in jq's order: output flags (color on for a
    // terminal unless NO_COLOR; -C, then -M), the JQ_COLORS warning, the
    // default program or usage, then the -f file.
    let stdout_is_tty = io::stdout().is_terminal();
    let no_color = std::env::var_os("NO_COLOR");
    let dumpopts = opts.dumpopts(stdout_is_tty, no_color.as_deref().map(OsStrExt::as_bytes));
    if let Some(spec) = std::env::var_os("JQ_COLORS")
        && args::jq_colors(spec.as_bytes()).is_none()
    {
        eprint!("{}", args::JQ_COLORS_WARNING);
    }
    let Some(program) = opts.program_or_default(io::stdin().is_terminal(), stdout_is_tty) else {
        fail(&ArgError::NoProgram);
    };
    let program = if opts.from_file {
        args::load_program(program).unwrap_or_else(|e| fail(&e))
    } else {
        program.to_vec()
    };
    let filter_str = String::from_utf8_lossy(&program).into_owned();

    // qj extension: expand glob patterns among the input files ('*.json.gz').
    let input_files: Vec<String> = args::expand_file_globs(&opts.files)
        .iter()
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();

    let filter = match qj::filter::parse(&filter_str) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("qj: error: failed to parse filter: {filter_str}\n\nCaused by:\n    {e}");
            std::process::exit(3);
        }
    };

    // Resolve module imports (import/include) if the filter uses them.
    // The module loader resolves all imports into the Env and strips
    // the import/include nodes from the filter AST.
    let library_paths: Vec<std::path::PathBuf> = opts
        .lib_search_paths
        .iter()
        .flatten()
        .map(|p| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(p)))
        .collect();
    let (filter, module_loader) = if !library_paths.is_empty() || has_module_stmts(&filter) {
        let search_paths = library_paths;
        let mut loader = qj::filter::module::ModuleLoader::new(search_paths.clone());
        match loader.resolve(&filter, qj::filter::Env::empty()) {
            Ok((resolved_filter, module_env)) => {
                // Set module metadata cache for modulemeta builtin
                qj::filter::eval::set_module_metadata(
                    loader.export_metadata(),
                    search_paths.clone(),
                );
                (resolved_filter, Some((loader, module_env)))
            }
            Err(e) => {
                eprintln!("qj: error: {e:#}");
                std::process::exit(3);
            }
        }
    } else {
        (filter, None)
    };

    // --stream-errors implies --stream (opts.stream is set for both).
    let effective_stream = opts.stream;

    // --stream: wrap filter with `tostream |` for the common case (non-slurp, non-null-input).
    // For slurp and null-input, the expansion happens later at the value level.
    // For --stream-errors, keep the unwrapped filter for error entries.
    let unwrapped_filter = if opts.stream_errors && !opts.slurp && !opts.null_input {
        Some(filter.clone())
    } else {
        None
    };
    let filter = if effective_stream && !opts.slurp && !opts.null_input {
        qj::filter::Filter::Pipe(
            Box::new(qj::filter::Filter::Builtin("tostream".to_string(), vec![])),
            Box::new(filter),
        )
    } else {
        filter
    };

    // Bind the variables main.c passes to jq_compile_args. Variable names in
    // the AST include the '$' prefix (e.g., "$name").
    let mut env = if let Some((_, ref module_env)) = module_loader {
        module_env.clone()
    } else {
        qj::filter::Env::empty()
    };
    for var in opts.program_arguments() {
        match var {
            // jq resolves `$ENV` before named arguments; `--arg ENV x` only
            // shows up in $ARGS.named.
            ProgramArgument::Named(b"ENV", _) => {}
            ProgramArgument::Named(name, value) => {
                let name = format!("${}", String::from_utf8_lossy(name));
                env = env.bind_var(name, arg_value(value));
            }
            ProgramArgument::Args => {
                let positional = opts.positional.iter().map(arg_value).collect();
                let named = opts
                    .named
                    .iter()
                    .map(|(n, v)| (String::from_utf8_lossy(n).into_owned(), arg_value(v)))
                    .collect();
                let args_obj = qj::value::Value::Object(Arc::new(vec![
                    (
                        "positional".to_string(),
                        qj::value::Value::Array(Arc::new(positional)),
                    ),
                    (
                        "named".to_string(),
                        qj::value::Value::Object(Arc::new(named)),
                    ),
                ]));
                env = env.bind_var("$ARGS".to_string(), args_obj);
            }
            ProgramArgument::BuildConfiguration => {
                env = env.bind_var(
                    "$JQ_BUILD_CONFIGURATION".to_string(),
                    qj::value::Value::String(qj::cli::usage::BUILD_CONFIGURATION.to_string()),
                );
            }
        }
    }

    let use_color = dumpopts & print_flags::COLOR != 0;
    let color_scheme = if use_color {
        qj::output::ColorScheme::jq_default()
    } else {
        qj::output::ColorScheme::none()
    };

    let stdout = io::stdout().lock();
    let mut out = BufWriter::with_capacity(128 * 1024, stdout);

    // jq's process(): with -r (also set by -j and --raw-output0), strings are
    // written raw; -j and --raw-output0 drop the newline, and --raw-output0
    // writes a NUL after every output.
    let pretty = dumpopts & print_flags::PRETTY != 0;
    let join_output = opts.raw_no_lf && !opts.raw_output0;
    let config = if opts.raw_output {
        qj::output::OutputConfig {
            mode: qj::output::OutputMode::Raw,
            indent: String::new(),
            sort_keys: opts.sorted_output,
            join_output,
            color: color_scheme,
            null_separator: opts.raw_output0,
            ascii_output: opts.ascii_output,
            unbuffered: opts.unbuffered_output,
            seq: opts.seq,
        }
    } else if !pretty {
        qj::output::OutputConfig {
            mode: qj::output::OutputMode::Compact,
            indent: String::new(),
            sort_keys: opts.sorted_output,
            join_output,
            color: color_scheme,
            null_separator: false,
            ascii_output: opts.ascii_output,
            unbuffered: opts.unbuffered_output,
            seq: opts.seq,
        }
    } else {
        qj::output::OutputConfig {
            mode: qj::output::OutputMode::Pretty,
            indent: if dumpopts & print_flags::TAB != 0 {
                "\t".to_string()
            } else {
                " ".repeat(print_flags::indent_width(dumpopts) as usize)
            },
            sort_keys: opts.sorted_output,
            join_output,
            color: color_scheme,
            null_separator: false,
            ascii_output: opts.ascii_output,
            unbuffered: opts.unbuffered_output,
            seq: opts.seq,
        }
    };

    // Detect passthrough-eligible patterns. Disable when semantic-changing
    // flags are active (slurp, raw_input, sort_keys, join_output) or when
    // color is enabled (passthrough bypasses the output formatter).
    // Also disable when -e is active — we need full eval to inspect output values.
    let passthrough = if opts.slurp
        || opts.raw_input
        || opts.sorted_output
        || opts.raw_no_lf
        || use_color
        || opts.ascii_output
        || opts.raw_output
        || opts.exit_status
        || effective_stream
        || opts.seq
    {
        None
    } else {
        qj::filter::passthrough_path(&filter).filter(|p| !p.requires_compact() || !pretty)
    };

    let uses_input = filter.uses_input_builtins();
    let ctx = ProcessCtx {
        passthrough: &passthrough,
        force_jsonl: opts.jsonl,
        exit_status: opts.exit_status,
        filter: &filter,
        env: &env,
        config: &config,
        debug_timing: opts.debug_timing,
    };
    let mut had_output = false;
    let mut had_error = false;
    let mut last_was_falsy = false;

    if opts.null_input {
        // With -n: collect all input values into the input queue (for input/inputs),
        // then eval with null input.
        if uses_input {
            let mut values = Vec::new();
            if !input_files.is_empty() {
                for path in &input_files {
                    if opts.raw_input {
                        let content = read_file_text(path)?;
                        for line in content.lines() {
                            values.push(qj::value::Value::String(line.to_string()));
                        }
                    } else if opts.seq {
                        let buf = read_input_bytes(path)?;
                        collect_seq_values(&buf, &mut values)?;
                    } else {
                        collect_file_values(path, opts.jsonl, &mut values)?;
                    }
                }
            } else {
                let mut buf = Vec::new();
                io::stdin()
                    .read_to_end(&mut buf)
                    .context("failed to read stdin")?;
                if opts.raw_input {
                    let text = std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
                    for line in text.lines() {
                        values.push(qj::value::Value::String(line.to_string()));
                    }
                } else if opts.seq {
                    collect_seq_values(&buf, &mut values)?;
                } else {
                    qj::input::strip_bom(&mut buf);
                    qj::input::collect_values_from_buf(&buf, opts.jsonl, &mut values)?;
                }
            }
            let values = if effective_stream {
                stream_expand_values(&values)
            } else {
                values
            };
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
    } else if opts.raw_input {
        // --raw-input: read lines as strings instead of parsing JSON
        if input_files.is_empty() {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read stdin")?;
            let text = std::str::from_utf8(&buf).context("stdin is not valid UTF-8")?;
            process_raw_input(
                text,
                opts.slurp,
                &filter,
                &env,
                &mut out,
                &config,
                &mut had_output,
                &mut had_error,
                &mut last_was_falsy,
            )?;
        } else if opts.slurp {
            // --raw-input --slurp with files: concatenate all file contents
            // into a single string (matches jq -Rs behavior)
            let mut all_text = String::new();
            for path in &input_files {
                let content = read_file_text(path)?;
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
                let content = read_file_text(path)?;
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
    } else if opts.seq {
        // --seq: RS-delimited (RFC 7464) input
        let mut values = Vec::new();
        if input_files.is_empty() {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read stdin")?;
            collect_seq_values(&buf, &mut values)?;
        } else {
            for path in &input_files {
                let buf = read_input_bytes(path)?;
                collect_seq_values(&buf, &mut values)?;
            }
        }
        if opts.slurp {
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
        } else {
            for value in &values {
                eval_and_output(
                    &filter,
                    value,
                    &env,
                    &mut out,
                    &config,
                    &mut had_output,
                    &mut had_error,
                    &mut last_was_falsy,
                );
            }
        }
    } else if opts.stream_errors && !opts.slurp && !opts.null_input {
        // --stream-errors: like --stream but parse errors become ["error msg", []] entries
        let error_filter = unwrapped_filter.as_ref().unwrap();
        let mut bufs: Vec<Vec<u8>> = Vec::new();
        if input_files.is_empty() {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read stdin")?;
            qj::input::strip_bom(&mut buf);
            bufs.push(buf);
        } else {
            for path in &input_files {
                bufs.push(read_input_bytes(path)?);
            }
        }
        for buf in &bufs {
            if buf
                .iter()
                .all(|&b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
            {
                continue;
            }
            for result in parse_docs_with_errors(buf) {
                match result {
                    Ok(value) => {
                        // Success: apply wrapped filter (tostream | user_filter)
                        eval_and_output(
                            &filter,
                            &value,
                            &env,
                            &mut out,
                            &config,
                            &mut had_output,
                            &mut had_error,
                            &mut last_was_falsy,
                        );
                    }
                    Err(msg) => {
                        // Parse error: create error entry and apply unwrapped filter
                        let error_entry = make_stream_error_entry(&msg);
                        eval_and_output(
                            error_filter,
                            &error_entry,
                            &env,
                            &mut out,
                            &config,
                            &mut had_output,
                            &mut had_error,
                            &mut last_was_falsy,
                        );
                    }
                }
            }
        }
    } else if opts.slurp {
        // --slurp: collect all values into an array, eval once
        let mut values = Vec::new();
        if input_files.is_empty() {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read stdin")?;
            qj::input::strip_bom(&mut buf);
            qj::input::collect_values_from_buf(&buf, opts.jsonl, &mut values)?;
        } else {
            for path in &input_files {
                collect_file_values(path, opts.jsonl, &mut values)?;
            }
        }
        let values = if effective_stream {
            stream_expand_values(&values)
        } else {
            values
        };
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
        let mut buf = read_stdin()?;
        qj::input::strip_bom(&mut buf);
        if uses_input {
            // Empty input produces no output (matches jq behavior)
            if !is_blank(&buf) {
                // Collect all values; first becomes input, rest go to queue
                let mut values = Vec::new();
                qj::input::collect_values_from_buf(&buf, opts.jsonl, &mut values)?;
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
            }
        } else {
            process_buffer(
                &buf,
                &ctx,
                &mut out,
                &mut had_output,
                &mut had_error,
                &mut last_was_falsy,
            )?;
        }
    } else {
        // files
        if uses_input {
            // Collect all values from all files; first becomes input, rest go to queue
            let mut values = Vec::new();
            for path in &input_files {
                collect_file_values(path, opts.jsonl, &mut values)?;
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
                        eprintln!("qj: error: {}", file_error_message(path, &e));
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

    if opts.exit_status {
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

/// Create a `--stream-errors` error entry: `["error message", []]`.
fn make_stream_error_entry(msg: &str) -> qj::value::Value {
    qj::value::Value::Array(Arc::new(vec![
        qj::value::Value::String(msg.to_string()),
        qj::value::Value::Array(Arc::new(vec![])),
    ]))
}

/// Parse a buffer as one or more JSON documents for `--stream-errors`.
/// Each successfully parsed document is wrapped in `Ok(value)`.
/// Parse failures produce `Err(error_message)`.
/// For NDJSON, each line is tried independently.
/// Try to parse a single JSON document, validating first to catch malformed input.
fn parse_single_doc(trimmed: &[u8]) -> std::result::Result<qj::value::Value, String> {
    let padded = qj::simdjson::pad_buffer(trimmed);
    // Validate first — dom_parse_to_value may silently accept non-JSON tokens
    if let Err(e) = qj::simdjson::dom_validate(&padded, trimmed.len()) {
        return Err(format!("{e}"));
    }
    qj::simdjson::dom_parse_to_value(&padded, trimmed.len()).map_err(|e| format!("{e}"))
}

fn parse_docs_with_errors(buf: &[u8]) -> Vec<std::result::Result<qj::value::Value, String>> {
    let mut results = Vec::new();
    // Count non-empty lines to determine if this is multi-doc input
    let non_empty_lines = buf
        .split(|&b| b == b'\n')
        .filter(|line| line.iter().any(|b| !b.is_ascii_whitespace()))
        .count();

    if non_empty_lines > 1 {
        // Multi-doc: parse each line independently (error recovery per line)
        for line in buf.split(|&b| b == b'\n') {
            let trimmed =
                line.iter()
                    .position(|b| !b.is_ascii_whitespace())
                    .map_or(&[] as &[u8], |start| {
                        let end = line
                            .iter()
                            .rposition(|b| !b.is_ascii_whitespace())
                            .unwrap_or(start);
                        &line[start..=end]
                    });
            if trimmed.is_empty() {
                continue;
            }
            results.push(parse_single_doc(trimmed));
        }
    } else {
        // Single document
        let trimmed = buf
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map_or(buf, |start| {
                let end = buf
                    .iter()
                    .rposition(|b| !b.is_ascii_whitespace())
                    .unwrap_or(start);
                &buf[start..=end]
            });
        if !trimmed.is_empty() {
            results.push(parse_single_doc(trimmed));
        }
    }
    results
}

/// Expand each value through `tostream`, collecting all stream entries.
/// Used for `--stream` in slurp and null-input modes where we can't wrap the filter.
fn stream_expand_values(values: &[qj::value::Value]) -> Vec<qj::value::Value> {
    let tostream = qj::filter::Filter::Builtin("tostream".to_string(), vec![]);
    let mut expanded = Vec::new();
    for value in values {
        qj::filter::eval::eval_filter(&tostream, value, &mut |v| expanded.push(v));
    }
    expanded
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
    exit_status: bool,
    filter: &'a qj::filter::Filter,
    env: &'a qj::filter::Env,
    config: &'a qj::output::OutputConfig,
    debug_timing: bool,
}

/// Whether a buffer has nothing but JSON whitespace.
fn is_blank(buf: &[u8]) -> bool {
    buf.iter()
        .all(|&b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Process input read into memory (stdin, or a `-` file argument): NDJSON in
/// parallel, a passthrough, or the normal pipeline.
fn process_buffer(
    buf: &[u8],
    ctx: &ProcessCtx,
    out: &mut impl Write,
    had_output: &mut bool,
    had_error: &mut bool,
    last_was_falsy: &mut bool,
) -> Result<()> {
    // Empty input produces no output (matches jq behavior)
    if is_blank(buf) {
        return Ok(());
    }
    if !ctx.exit_status && (ctx.force_jsonl || qj::parallel::ndjson::is_ndjson(buf)) {
        let (output, ho, errs) =
            qj::parallel::ndjson::process_ndjson(buf, ctx.filter, ctx.config, ctx.env)
                .context("failed to process NDJSON from stdin")?;
        out.write_all(&output)?;
        *had_output |= ho;
        if !errs.is_empty() {
            // Always surface per-line errors to stderr (matching jq).
            // Only set had_error (exit 5) when no output was produced —
            // jq exits 0 for mixed success/error NDJSON.
            if !ho {
                *had_error = true;
            }
            io::stderr().write_all(&errs)?;
        }
        return Ok(());
    }
    let json_len = buf.len();
    let padded = qj::simdjson::pad_buffer(buf);
    if let Some(pt) = ctx.passthrough
        && try_passthrough(&padded, json_len, pt, out, had_output).context("passthrough failed")?
    {
        return Ok(());
    }
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
    )
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
    if path == "-" {
        let mut buf = read_stdin()?;
        qj::input::strip_bom(&mut buf);
        return process_buffer(&buf, ctx, out, had_output, had_error, last_was_falsy);
    }

    // ---- Compressed file handling ----
    // Decompress to memory, then process the decompressed buffer.
    // Can't use mmap or streaming NDJSON directly on compressed data.
    if qj::decompress::is_compressed(path) {
        let decompressed = qj::decompress::decompress_file(path)?;
        if decompressed.is_empty() {
            return Ok(());
        }

        // NDJSON: use Cursor as a Read+Seek source for the streaming processor
        if !ctx.debug_timing && (ctx.force_jsonl || qj::parallel::ndjson::is_ndjson(&decompressed))
        {
            let mut cursor = std::io::Cursor::new(decompressed);
            let ho = qj::parallel::ndjson::process_ndjson_streaming(
                &mut cursor,
                ctx.filter,
                ctx.config,
                ctx.env,
                out,
            )
            .with_context(|| format!("failed to process NDJSON: {path}"))?;
            *had_output |= ho;
            return Ok(());
        }

        // Single doc: pad and process
        let json_len = decompressed.len();
        let padded = qj::simdjson::pad_buffer(&decompressed);

        std::str::from_utf8(&padded[..json_len])
            .with_context(|| format!("file is not valid UTF-8: {path}"))?;

        if let Some(pt) = ctx.passthrough {
            let handled = try_passthrough(&padded, json_len, pt, out, had_output)
                .with_context(|| format!("passthrough failed: {path}"))?;
            if handled {
                return Ok(());
            }
        }

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
        return Ok(());
    }

    // ---- Uncompressed file handling ----

    // NDJSON: mmap the file directly (no simdjson padding needed) and process
    // in parallel windows. Falls back to streaming read() if mmap is unavailable.
    // Works for files larger than physical RAM — kernel pages in on demand.
    if !ctx.debug_timing
        && let Some(ho) = qj::parallel::ndjson::process_ndjson_file(
            std::path::Path::new(path),
            ctx.filter,
            ctx.config,
            ctx.env,
            ctx.force_jsonl,
            out,
        )
        .with_context(|| format!("failed to process NDJSON: {path}"))?
    {
        *had_output |= ho;
        return Ok(());
    }

    // Non-NDJSON: load via read_padded_file (mmap with simdjson padding).
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
            // Try special float preprocessing (NaN, Infinity, nan, inf)
            let raw = &padded[..json_len];
            if qj::input::has_special_float_tokens_pub(raw) {
                let pp = qj::input::preprocess_special_floats_pub(raw);
                let pp_padded = qj::simdjson::pad_buffer(&pp);
                if let Ok(val) = qj::simdjson::dom_parse_to_value_fast(&pp_padded, pp.len()) {
                    let input = qj::input::fixup_special_float_sentinels_pub(val);
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
                    return Ok(());
                }
            }
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
