//! Port of jq 1.8.1's `linker.c`: `import`/`include`, module search paths, data
//! imports, `modulemeta`'s `load_module_meta`, and `load_program` (which also adds
//! jq's implicit `~/.jq` include).
//!
//! Every message is jq's, including its quirks: "module not found" errors are
//! reported with a trailing newline (so jq prints an empty line after them), a code
//! module that is a directory is "error loading data file", and a missing module
//! stops the search at once (`process_dependencies` returns 1, whatever was
//! reported before).

use std::os::unix::ffi::OsStrExt;
use std::rc::Rc;

use super::bytecode::OP_IS_CALL_PSEUDO;
use super::compile::{Block, Compiler, LocFileId};
use super::locfile::LocFile;
use super::lower::{CompileHooks, Lowerer};
use super::parser::{parse, parse_library};
use crate::jq::value::{Error, Object, Str, Value, load_file};

/// The jq attributes (`jq_set_attr`) the linker reads, which the `get_search_list`,
/// `get_prog_origin`, `get_jq_origin` and `modulemeta` builtins also expose.
#[derive(Clone, Debug)]
pub struct JqAttrs {
    /// `JQ_LIBRARY_PATH`: the `-L` directories in order, or jq's default list
    /// ([`default_lib_dirs`]). Returned as-is by `get_search_list`.
    pub lib_dirs: Value,
    /// `JQ_ORIGIN`: `dirname(argv[0])` (not resolved through `$PATH`, so `"."` when
    /// jq is run as plain `jq`). Substituted for `$ORIGIN/` in search paths.
    pub jq_origin: Value,
    /// `PROGRAM_ORIGIN`: `realpath(".")` for a program on the command line,
    /// `realpath(dirname(file))` for `-f file`. Relative search paths of the main
    /// program's imports are resolved against it.
    pub prog_origin: Value,
    /// `$HOME` (`get_home`), for `~/` in search paths and the implicit `~/.jq`
    /// include; `None` when unset.
    pub home: Option<String>,
}

impl JqAttrs {
    /// jq's defaults for a program given on the command line, run from the current
    /// directory: the default search list, `$ORIGIN` = `jq_origin`,
    /// `PROGRAM_ORIGIN` = `realpath(".")`, and `$HOME` from the environment.
    pub fn new(jq_origin: &str) -> JqAttrs {
        JqAttrs {
            lib_dirs: default_lib_dirs(),
            jq_origin: Value::from(jq_origin),
            prog_origin: jq_realpath(Value::from(".")),
            home: std::env::var_os("HOME").map(|h| lossy(h.as_bytes())),
        }
    }
}

/// main.c's default search list: `["~/.jq", "$ORIGIN/../lib/jq", "$ORIGIN/../lib"]`.
pub fn default_lib_dirs() -> Value {
    Value::from(vec![
        Value::from("~/.jq"),
        Value::from("$ORIGIN/../lib/jq"),
        Value::from("$ORIGIN/../lib"),
    ])
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The C `%s` of a jq string: up to the first NUL.
fn cstr(s: &str) -> &str {
    match s.find('\0') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// util.c `jq_realpath`: `realpath(path)`, or `path` unchanged when that fails.
pub fn jq_realpath(path: Value) -> Value {
    let Some(p) = path.as_str() else {
        return path;
    };
    match std::fs::canonicalize(std::ffi::OsStr::from_bytes(cstr(p).as_bytes())) {
        Ok(r) => Value::string_from_bytes(r.as_os_str().as_bytes()),
        Err(_) => path,
    }
}

/// POSIX `dirname` (as used by main.c and linker.c).
pub fn dirname(p: &str) -> String {
    let b = p.as_bytes();
    if b.is_empty() {
        return ".".into();
    }
    let mut end = b.len();
    while end > 1 && b[end - 1] == b'/' {
        end -= 1;
    }
    match b[..end].iter().rposition(|&c| c == b'/') {
        None => ".".into(),
        Some(i) => {
            let mut e = i;
            while e > 0 && b[e - 1] == b'/' {
                e -= 1;
            }
            if e == 0 { "/".into() } else { lossy(&b[..e]) }
        }
    }
}

/// util.c `expand_path`: `~/` becomes `$HOME/`.
fn expand_path(attrs: &JqAttrs, path: &str) -> Result<String, String> {
    let p = cstr(path);
    if p.len() > 1 && p.starts_with("~/") {
        return match &attrs.home {
            Some(home) => Ok(format!("{}/{}", cstr(home), &p[2..])),
            None => Err(format!(
                "Could not expand {p}. (Could not find home directory.)"
            )),
        };
    }
    Ok(path.to_string())
}

/// `build_lib_search_chain`: expands `~/`, `$ORIGIN/` and relative entries (relative
/// to `lib_origin`, except `.` itself, which stays relative to the current
/// directory). Also returns the last expansion error.
fn build_lib_search_chain(
    attrs: &JqAttrs,
    search_path: &[Value],
    jq_origin: &Value,
    lib_origin: &Value,
) -> (Vec<String>, Option<String>) {
    let mut expanded = Vec::new();
    let mut err = None;
    for path in search_path {
        let Some(path) = path.as_str() else {
            continue;
        };
        let path = match expand_path(attrs, path) {
            Ok(p) => p,
            Err(e) => {
                err = Some(e);
                continue;
            }
        };
        let p = cstr(&path);
        let elt = if p == "." {
            path.clone()
        } else if let Some(rest) = p.strip_prefix("$ORIGIN/") {
            format!("{}/{}", cstr(jq_origin.as_str().unwrap_or("")), rest)
        } else if let Some(origin) = lib_origin.as_str()
            && !p.starts_with('/')
        {
            format!("{}/{}", cstr(origin), p)
        } else {
            path.clone()
        };
        expanded.push(elt);
    }
    (expanded, err)
}

/// `validate_relpath`.
fn validate_relpath(name: &str) -> Result<String, String> {
    let s = cstr(name);
    if s.contains('\\') {
        return Err(format!(
            "Modules must be named by relative paths using '/', not '\\' ({s})"
        ));
    }
    let components: Vec<&str> = name.split('/').collect();
    for (i, x) in components.iter().enumerate() {
        if *x == ".." {
            return Err(format!(
                "Relative paths to modules may not traverse to parent directories ({s})"
            ));
        }
        if i > 0 && *x == components[i - 1] {
            return Err(format!(
                "module names must not have equal consecutive components: {s}"
            ));
        }
    }
    Ok(name.to_string())
}

/// `jv_basename` (of a validated relative path): from the last `/` on (inclusive).
fn jv_basename(name: &str) -> &str {
    let s = cstr(name);
    match s.rfind('/') {
        Some(i) => &s[i..],
        None => name,
    }
}

/// `stat(path)`: `Ok` if it exists, else whether the error was `ENOENT`.
fn stat(path: &str) -> Result<(), bool> {
    match std::fs::metadata(std::ffi::OsStr::from_bytes(cstr(path).as_bytes())) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.raw_os_error() == Some(libc::ENOENT)),
    }
}

/// `find_lib`: the resolved path of module `rel_path`, trying for each search
/// directory `dir/rel_path<suffix>`, `dir/rel_path/jq/main<suffix>`, and
/// `dir/rel_path/<basename><suffix>`.
fn find_lib(
    attrs: &JqAttrs,
    rel_path: Result<String, String>,
    search: &Value,
    suffix: &str,
    jq_origin: &Value,
    lib_origin: &Value,
) -> Result<String, String> {
    let rel_path = rel_path?;
    let Some(search) = search.as_array() else {
        return Err("Module search path must be an array".into());
    };
    let (search, err) = build_lib_search_chain(attrs, search.as_slice(), jq_origin, lib_origin);
    let rp = cstr(&rel_path);
    let bname = jv_basename(&rel_path);
    for spath in &search {
        if spath.is_empty() {
            continue; /* XXX report non-strings in search path?? */
        }
        let sp = cstr(spath);
        let mut testpath = realpath_str(&format!("{sp}/{rp}{suffix}"));
        let mut ret = stat(&testpath);
        if ret == Err(true) {
            testpath = realpath_str(&format!("{sp}/{rp}/jq/main{suffix}"));
            ret = stat(&testpath);
        }
        if ret == Err(true) {
            testpath = realpath_str(&format!("{sp}/{rp}/{}{suffix}", cstr(bname)));
            ret = stat(&testpath);
        }
        if ret.is_ok() {
            return Ok(testpath);
        }
    }
    Err(match err {
        Some(e) => format!("module not found: {rp} ({e})"),
        None => format!("module not found: {rp}"),
    })
}

fn realpath_str(p: &str) -> String {
    match jq_realpath(Value::from(p)) {
        Value::String(s) => s.as_str().to_string(),
        _ => p.to_string(),
    }
}

/// `default_search`: an import's `search` metadata as an array, or `["."]` plus the
/// library path when it has none.
fn default_search(attrs: &JqAttrs, value: Option<&Value>) -> Value {
    match value {
        None => {
            let mut v = vec![Value::from(".")];
            if let Some(dirs) = attrs.lib_dirs.as_array() {
                v.extend(dirs.iter().cloned());
            }
            Value::from(v)
        }
        Some(v @ Value::Array(_)) => v.clone(),
        Some(v) => Value::from(vec![v.clone()]),
    }
}

/// `lib_loading_state`: modules loaded so far (by resolved path) and their
/// definitions.
#[derive(Default)]
struct LibState {
    names: Vec<String>,
    defs: Vec<Block>,
}

/// `jq_parse`: parses (reporting syntax errors) and lowers a program.
pub fn jq_parse(c: &mut Compiler, lf: LocFileId) -> Result<Block, usize> {
    let locfile = c.locfile(lf).clone();
    let parsed = {
        let mut hooks = CompileHooks { c, lf };
        parse(locfile.data(), &mut hooks)
    };
    match parsed {
        Ok(program) => Ok(Lowerer::new(c, lf).lower_program(&program)),
        Err(errors) => {
            for e in &errors {
                c.report(e.render(&locfile));
            }
            Err(errors.len())
        }
    }
}

/// `jq_parse_library`: like [`jq_parse`], but only definitions (and directives) are
/// allowed.
pub fn jq_parse_library(c: &mut Compiler, lf: LocFileId) -> Result<Block, usize> {
    let locfile = c.locfile(lf).clone();
    let parsed = {
        let mut hooks = CompileHooks { c, lf };
        parse_library(locfile.data(), &mut hooks)
    };
    match parsed {
        Ok(program) => Ok(Lowerer::new(c, lf).lower_program(&program)),
        Err(errors) => {
            for e in &errors {
                c.report(e.render(&locfile));
            }
            Err(errors.len())
        }
    }
}

fn is_true(o: &Object, key: &str) -> bool {
    matches!(o.get(key), Some(Value::Bool(true)))
}

/// `process_dependencies`: resolves, loads and binds the imports at the start of
/// `src_block` (removing them), last import first.
fn process_dependencies(
    c: &mut Compiler,
    attrs: &JqAttrs,
    jq_origin: &Value,
    lib_origin: &Value,
    src_block: &mut Block,
    lib_state: &mut LibState,
) -> usize {
    let deps = c.block_take_imports(src_block);
    let mut bk = *src_block;
    let mut nerrors = 0;

    // XXX This is a backward jv_array_foreach because bindings go in reverse
    for dep in deps.iter().rev() {
        let Value::Object(dep) = dep else {
            continue;
        };
        let is_data = is_true(dep, "is_data");
        let raw = is_true(dep, "raw");
        let optional = is_true(dep, "optional");
        let relpath = match dep.get("relpath") {
            Some(Value::String(s)) => validate_relpath(s.as_str()),
            _ => Err("Module path must be a string".into()),
        };
        let as_str = dep.get("as").and_then(|v| v.as_str()).map(str::to_string);
        let search = default_search(attrs, dep.get("search"));

        let resolved = find_lib(
            attrs,
            relpath,
            &search,
            if is_data { ".json" } else { ".jq" },
            jq_origin,
            lib_origin,
        );
        let resolved = match resolved {
            Ok(r) => r,
            Err(emsg) => {
                if optional {
                    continue;
                }
                c.report(format!("jq: error: {}\n", cstr(&emsg)));
                return 1;
            }
        };

        if is_data {
            // Can't reuse data libs because the wrong name is bound
            let (n, dep_def_block) = load_library(
                c,
                attrs,
                resolved,
                is_data,
                raw,
                optional,
                as_str.as_deref(),
                lib_state,
            );
            nerrors += n;
            if nerrors == 0 {
                // Bind as both $data::data and $data for backward compatibility vs
                // common sense
                bk = c.block_bind_library(dep_def_block, bk, OP_IS_CALL_PSEUDO, as_str.as_deref());
                bk = c.block_bind_library(dep_def_block, bk, OP_IS_CALL_PSEUDO, None);
            }
        } else if let Some(idx) = lib_state.names.iter().position(|n| *n == resolved) {
            // Bind the library to the program
            let defs = lib_state.defs[idx];
            bk = c.block_bind_library(defs, bk, OP_IS_CALL_PSEUDO, as_str.as_deref());
        } else {
            // Not found. Add it to the table before binding.
            let (n, dep_def_block) = load_library(
                c,
                attrs,
                resolved,
                is_data,
                raw,
                optional,
                as_str.as_deref(),
                lib_state,
            );
            nerrors += n;
            if nerrors == 0 {
                // Bind the library to the program
                bk = c.block_bind_library(dep_def_block, bk, OP_IS_CALL_PSEUDO, as_str.as_deref());
            }
        }
    }
    let _ = bk;
    nerrors
}

/// `load_library`: loads the module (or data file) at `lib_path` into `lib_state`,
/// returning the error count and its definitions.
#[allow(clippy::too_many_arguments)]
fn load_library(
    c: &mut Compiler,
    attrs: &JqAttrs,
    lib_path: String,
    is_data: bool,
    raw: bool,
    optional: bool,
    as_: Option<&str>,
    lib_state: &mut LibState,
) -> (usize, Block) {
    let mut nerrors = 0;
    // `jv_load_file(path, 0)` parses JSON only for (non-raw) data imports.
    let data = load_file(cstr(&lib_path), !is_data || raw);
    let program = match data {
        Err(msg) => {
            if !optional {
                let msg = match msg.value() {
                    Value::String(s) => s.as_str().to_string(),
                    _ => "unknown error".to_string(),
                };
                c.report(format!(
                    "jq: error loading data file {}: {}\n",
                    cstr(&lib_path),
                    cstr(&msg)
                ));
                nerrors += 1;
            }
            return (nerrors, Block::NOOP);
        }
        // import "foo" as $bar;
        Ok(data) if is_data => c.gen_const_global(data, as_.unwrap_or("")),
        // import "foo" as bar;
        Ok(data) => {
            let text = match &data {
                Value::String(s) => s.as_bytes().to_vec(),
                _ => Vec::new(),
            };
            let lf = c.add_locfile(Rc::new(LocFile::new(&lib_path, &text)));
            match jq_parse_library(c, lf) {
                Err(n) => {
                    nerrors += n;
                    Block::NOOP
                }
                Ok(mut program) => {
                    let lib_origin = Value::from(dirname(&lib_path));
                    nerrors += process_dependencies(
                        c,
                        attrs,
                        &attrs.jq_origin,
                        &lib_origin,
                        &mut program,
                        lib_state,
                    );
                    c.block_bind_self(program, OP_IS_CALL_PSEUDO)
                }
            }
        }
    };
    lib_state.names.push(lib_path);
    lib_state.defs.push(program);
    (nerrors, program)
}

/// `load_module_meta` (`modulemeta`): the module's metadata object plus `deps` (its
/// imports) and `defs` (its definitions as `name/arity`). Syntax errors in the
/// module are reported through `report` (jq prints them) and give `null`.
pub fn load_module_meta(
    attrs: &JqAttrs,
    mod_relpath: &str,
    report: &mut dyn FnMut(String),
) -> Result<Value, Error> {
    // We can't know the caller's origin; we could though, if it was passed in
    let lib_path = find_lib(
        attrs,
        validate_relpath(mod_relpath),
        &attrs.lib_dirs,
        ".jq",
        &attrs.jq_origin,
        &Value::Null,
    )
    .map_err(Error::msg)?;
    let mut meta = Value::Null;
    if let Ok(data) = load_file(cstr(&lib_path), true) {
        let text = match &data {
            Value::String(s) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        };
        let mut c = Compiler::new();
        let lf = c.add_locfile(Rc::new(LocFile::new(&lib_path, &text)));
        if let Ok(mut program) = jq_parse_library(&mut c, lf) {
            let mut m = match c.block_module_meta(program) {
                Value::Object(o) => o,
                _ => Object::new(),
            };
            let deps = c.block_take_imports(&mut program);
            m.insert(Str::from("deps"), Value::from(deps));
            let defs: Vec<Value> = c
                .block_list_funcs(program, false)
                .into_iter()
                .map(Value::from)
                .collect();
            m.insert(Str::from("defs"), Value::from(defs));
            meta = Value::Object(m);
        }
        for msg in c.messages.drain(..) {
            report(msg);
        }
    }
    Ok(meta)
}

/// `load_program`: parses the main program, adds the implicit `~/.jq` include,
/// loads and binds its modules, and drops unreferenced library definitions.
pub fn load_program(c: &mut Compiler, attrs: &JqAttrs, lf: LocFileId) -> Result<Block, usize> {
    let mut program = jq_parse(c, lf)?;

    if !c.block_has_main(program) {
        c.report("jq: error: Top-level program not given (try \".\")".into());
        return Err(1);
    }

    if let Some(home) = &attrs.home {
        /* Import ~/.jq as a library named "" found in $HOME or %USERPROFILE% */
        let import = c.gen_import("", None, false);
        let mut meta = Object::new();
        meta.insert(Str::from("optional"), Value::Bool(true));
        meta.insert(Str::from("search"), Value::from(home.as_str()));
        let meta = c.gen_const(Value::Object(meta));
        let import = c.gen_import_meta(import, meta);
        program = c.block_join(import, program);
    }

    let mut lib_state = LibState::default();
    let nerrors = process_dependencies(
        c,
        attrs,
        &attrs.jq_origin,
        &attrs.prog_origin,
        &mut program,
        &mut lib_state,
    );
    if nerrors > 0 {
        return Err(nerrors);
    }
    let mut libs = Block::NOOP;
    for defs in lib_state.defs {
        if !c.block_is_const(defs) {
            libs = c.block_join(libs, defs);
        }
    }
    let all = c.block_join(libs, program);
    Ok(c.block_drop_unreferenced(all))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirname_is_posix() {
        for (p, d) in [
            ("", "."),
            ("a", "."),
            ("/", "/"),
            ("//", "/"),
            ("/a", "/"),
            ("a/b", "a"),
            ("a/b/", "a"),
            ("a//b", "a"),
            ("/a/b/c", "/a/b"),
            ("//a//b//", "//a"),
            ("..", "."),
        ] {
            assert_eq!(dirname(p), d, "dirname({p:?})");
        }
    }

    #[test]
    fn relpath_validation() {
        assert!(validate_relpath("a/b").is_ok());
        assert_eq!(
            validate_relpath("a\\b").unwrap_err(),
            "Modules must be named by relative paths using '/', not '\\' (a\\b)"
        );
        assert_eq!(
            validate_relpath("a/../b").unwrap_err(),
            "Relative paths to modules may not traverse to parent directories (a/../b)"
        );
        assert_eq!(
            validate_relpath("a/a").unwrap_err(),
            "module names must not have equal consecutive components: a/a"
        );
        assert_eq!(jv_basename("a/b"), "/b");
        assert_eq!(jv_basename("ab"), "ab");
    }
}
