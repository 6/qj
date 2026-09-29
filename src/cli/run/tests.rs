//! Tests for main.c's pieces that don't need a process (the binary's behavior
//! is covered by jq_diff). Expectations come from jq 1.8.1.

use super::*;

fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[test]
fn slurpfile_reads_4096_byte_chunks_like_jv_load_file() {
    // A 4096-byte file never gets a final, non-partial buffer, so a number
    // at its very end is dropped: `jq --slurpfile x f -n '$x|length'` is 2047.
    let mut data = b("1 ").repeat(2047);
    data.extend_from_slice(b"12");
    assert_eq!(data.len(), 4096);
    let v = load_file_data(&data, false).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2047);
    // One byte shorter or longer, the last read is short and the number is
    // kept.
    let v = load_file_data(&data[..4095], false).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2048);
    data.push(b' ');
    let v = load_file_data(&data, false).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2048);
    // printf 123 > f; jq --slurpfile x f -n -c '$x'  => [123]
    assert_eq!(load_file_data(b"123", false).unwrap().to_json(), "[123]");
    assert_eq!(load_file_data(b"", false).unwrap().to_json(), "[]");
    // jq: Bad JSON in --slurpfile x f: Unfinished JSON term at EOF at line 1, column 3
    assert_eq!(
        load_file_data(b"[1,", false).unwrap_err().to_string(),
        "Unfinished JSON term at EOF at line 1, column 3"
    );
}

#[test]
fn rawfile_repairs_each_chunk() {
    // 4095 x's and a lead byte: the read is extended for the rest of the
    // sequence, finds EOF, and the byte becomes U+FFFD (length 4096).
    let mut data = vec![b'x'; 4095];
    data.push(0xe2);
    let v = load_file_data(&data, true).unwrap();
    let s = v.as_str().unwrap();
    assert_eq!(s.chars().count(), 4096);
    assert!(s.ends_with("x\u{FFFD}"));
    // A character across the 4096-byte boundary survives.
    let mut data = vec![b'y'; 4095];
    data.extend_from_slice("€z".as_bytes());
    let v = load_file_data(&data, true).unwrap();
    assert!(v.as_str().unwrap().ends_with("y€z"));
}

#[test]
fn exit_status_follows_main_c() {
    let mut o: Options<Value> = match args::parse(&[b("jq"), b(".")], &mut PortArgs) {
        Ok(Action::Run(o)) => o,
        _ => unreachable!(),
    };
    // Without -e: negative statuses (null/false, no output) are 0.
    for (ret, want) in [(0, 0), (-1, 0), (-4, 0), (2, 2), (5, 5), (300, 300)] {
        assert_eq!(exit_status(&o, (ret, 1)), want, "{ret}");
    }
    o.exit_status = true;
    // -e: the status's absolute value, or, with no output from the last
    // input, the last result seen (none: 4, false/null: 1, else 0).
    assert_eq!(exit_status(&o, (0, -1)), 0);
    assert_eq!(exit_status(&o, (-1, 0)), 1);
    assert_eq!(exit_status(&o, (-3, 0)), 3);
    assert_eq!(exit_status(&o, (5, 1)), 5);
    assert_eq!(exit_status(&o, (-4, -1)), 4);
    assert_eq!(exit_status(&o, (-4, 0)), 1);
    assert_eq!(exit_status(&o, (-4, 1)), 0);
    // abs(INT_MIN) stays INT_MIN (exit status 0).
    assert_eq!(exit_status(&o, (i32::MIN, 1)), i32::MIN);
}

#[test]
fn halt_error_codes_convert_like_c() {
    // jq -n '"x"|halt_error(1.9)' exits 1, (-3.5) exits 0 (3 with -e).
    assert_eq!(c_double_to_int(1.9), 1);
    assert_eq!(c_double_to_int(-3.5), -3);
    assert_eq!(c_double_to_int(256.0), 256);
    #[cfg(target_arch = "aarch64")]
    {
        // arm64 saturates: halt_error(1e10) exits 255 on macOS.
        assert_eq!(c_double_to_int(1e10), i32::MAX);
        assert_eq!(c_double_to_int(-1e10), i32::MIN);
        assert_eq!(c_double_to_int(f64::NAN), 0);
    }
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(c_double_to_int(1e10), i32::MIN);
        assert_eq!(c_double_to_int(f64::NAN), i32::MIN);
    }
}

#[test]
fn dumpopts_map_to_printer_options() {
    use print_flags::{ASCII, COLOR, PRETTY, SORTED, TAB, indent_flags};
    let colors = Colors::default();
    let d = dump_options(indent_flags(2), &colors);
    assert_eq!(d, DumpOptions::pretty());
    assert_eq!(dump_options(0, &colors), DumpOptions::compact());
    assert_eq!(
        dump_options(indent_flags(0), &colors).indent,
        Indent::Spaces(0)
    );
    assert_eq!(dump_options(TAB | PRETTY, &colors).indent, Indent::Tab);
    // TAB without PRETTY (debug messages under --tab) is compact.
    assert_eq!(dump_options(TAB, &colors).indent, Indent::Compact);
    let d = dump_options(SORTED | ASCII | COLOR, &colors);
    assert!(d.sort_keys && d.ascii && d.colors == Some(colors));
}

#[test]
fn named_arguments_and_args() {
    // jq --arg x 1 --arg x 2 --argjson y '{"a":2}' -n '$ARGS' --args a
    let argv: Vec<Vec<u8>> = [
        "jq",
        "--arg",
        "x",
        "1",
        "--arg",
        "x",
        "2",
        "--argjson",
        "y",
        "{\"a\":2}",
        "-n",
        "$ARGS",
        "--args",
        "a",
    ]
    .iter()
    .map(|s| b(s))
    .collect();
    let o = match args::parse(&argv, &mut PortArgs) {
        Ok(Action::Run(o)) => o,
        _ => unreachable!(),
    };
    let vars = Value::Object(program_arguments(&o));
    assert_eq!(
        vars.to_json(),
        format!(
            "{{\"x\":\"1\",\"y\":{{\"a\":2}},\"ARGS\":{{\"positional\":[\"a\"],\
             \"named\":{{\"x\":\"1\",\"y\":{{\"a\":2}}}}}},\"JQ_BUILD_CONFIGURATION\":{}}}",
            Value::from(crate::cli::usage::BUILD_CONFIGURATION).to_json()
        )
    );
}

#[test]
fn program_files_are_repaired_then_cut_at_nul() {
    let dir = std::env::temp_dir();
    let f = dir.join(format!("qj-run-program-{}.jq", std::process::id()));
    std::fs::write(&f, b".a | \"\xff\"\x00 garbage").unwrap();
    let text = load_program_text(f.as_os_str().as_bytes()).unwrap();
    assert_eq!(text, ".a | \"\u{FFFD}\"".as_bytes());
    std::fs::remove_file(&f).unwrap();
    let missing = dir.join("qj-run-program-missing.jq");
    match load_program_text(missing.as_os_str().as_bytes()) {
        Err(ArgError::ProgramFile(m)) => assert_eq!(
            String::from_utf8(m).unwrap(),
            format!(
                "Could not open {}: No such file or directory",
                missing.display()
            )
        ),
        other => panic!("{other:?}"),
    }
}
