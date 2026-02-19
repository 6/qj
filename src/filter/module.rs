/// Module loader for jq's import/include/modulemeta system.
///
/// Resolves module paths, loads and parses `.jq` and `.json` files,
/// handles transitive imports, and builds an `Env` with all imported
/// definitions.
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::{Env, Filter, UserFunc};
use crate::value::Value;
use std::sync::Arc;

/// A loaded module's definitions and metadata.
#[derive(Debug, Clone)]
pub struct LoadedModule {
    /// Module metadata from `module {…};` declaration (if present).
    pub metadata: Option<Value>,
    /// Function definitions: (name, arity, params, body).
    pub defs: Vec<(String, Vec<String>, Filter)>,
    /// Import/include dependencies as declared.
    pub deps: Vec<ModuleDep>,
    /// Resolved filesystem path.
    pub resolved_path: PathBuf,
}

/// A dependency declaration from `import`/`include` in a module file.
#[derive(Debug, Clone)]
pub struct ModuleDep {
    pub relpath: String,
    pub alias: Option<String>,
    pub is_data: bool,
    pub is_include: bool,
    pub search: Option<String>,
    pub metadata: Option<Value>,
}

/// Module loader with search paths and caching.
pub struct ModuleLoader {
    search_paths: Vec<PathBuf>,
    cache: HashMap<PathBuf, LoadedModule>,
    loading: HashSet<PathBuf>,
}

impl ModuleLoader {
    pub fn new(search_paths: Vec<PathBuf>) -> Self {
        Self {
            search_paths,
            cache: HashMap::new(),
            loading: HashSet::new(),
        }
    }

    /// Resolve all import/include statements in a filter AST into an `Env`.
    /// Returns the inner filter (with import/include nodes stripped) and the
    /// populated environment.
    pub fn resolve(&mut self, filter: &Filter, env: Env) -> Result<(Filter, Env)> {
        self.resolve_inner(filter, env, &self.search_paths.clone())
    }

    fn resolve_inner(
        &mut self,
        filter: &Filter,
        mut env: Env,
        search_paths: &[PathBuf],
    ) -> Result<(Filter, Env)> {
        match filter {
            Filter::ModuleDecl { rest, .. } => {
                // Module declarations in the top-level filter are ignored
                // (they're only meaningful inside .jq module files).
                self.resolve_inner(rest, env, search_paths)
            }
            Filter::Import {
                path,
                alias,
                is_data,
                metadata,
                rest,
            } => {
                // Compute search paths (metadata {search: "./"} overrides)
                let extra_search = metadata.as_ref().and_then(extract_search);

                if *is_data {
                    // Data import: load JSON file, bind as variable
                    let resolved = resolve_data_path(path, search_paths, extra_search.as_deref())?;
                    let data = load_json_file(&resolved)?;
                    // Bind as $alias (e.g., $d) and also $alias::member (e.g., $d::d)
                    // The namespace member is the alias without the $ prefix.
                    env = env.bind_var(alias.clone(), data.clone());
                    let member = alias.trim_start_matches('$');
                    let ns_var = format!("{}::{}", alias, member);
                    env = env.bind_var(ns_var, data);
                } else {
                    // Code import: load .jq file, bind defs under namespace
                    let effective_search = if let Some(ref s) = extra_search {
                        // Relative search path: resolve against each base search path
                        let mut paths = Vec::new();
                        for base in search_paths {
                            paths.push(base.join(s));
                        }
                        paths
                    } else {
                        search_paths.to_vec()
                    };
                    let module = self.load_module(path, &effective_search)?;
                    env = self.bind_module_defs(&module, Some(alias), env, search_paths)?;
                }

                self.resolve_inner(rest, env, search_paths)
            }
            Filter::Include {
                path,
                metadata,
                rest,
            } => {
                let extra_search = metadata.as_ref().and_then(extract_search);

                let effective_search = if let Some(ref s) = extra_search {
                    let mut paths = Vec::new();
                    for base in search_paths {
                        paths.push(base.join(s));
                    }
                    paths
                } else {
                    search_paths.to_vec()
                };

                let module = self.load_module(path, &effective_search)?;
                env = self.bind_module_defs(&module, None, env, search_paths)?;

                self.resolve_inner(rest, env, search_paths)
            }
            // Not a module statement — return the filter unchanged
            other => Ok((other.clone(), env)),
        }
    }

    /// Load a .jq module file, parsing it and caching the result.
    fn load_module(&mut self, path: &str, search_paths: &[PathBuf]) -> Result<LoadedModule> {
        let resolved = resolve_module_path(path, search_paths)?;

        // Check cache
        if let Some(module) = self.cache.get(&resolved) {
            return Ok(module.clone());
        }

        // Circular import detection
        if self.loading.contains(&resolved) {
            bail!("circular import detected: {}", resolved.display());
        }
        self.loading.insert(resolved.clone());

        // Read and parse the .jq file
        let source = std::fs::read_to_string(&resolved)
            .with_context(|| format!("failed to read module: {}", resolved.display()))?;
        let filter = super::parse(&source)
            .with_context(|| format!("failed to parse module {}: ", resolved.display()))?;

        // Extract module structure
        let module = self.extract_module(&filter, &resolved, search_paths)?;

        self.loading.remove(&resolved);
        self.cache.insert(resolved, module.clone());

        Ok(module)
    }

    /// Walk a parsed module AST to extract metadata, defs, and dependencies.
    fn extract_module(
        &mut self,
        filter: &Filter,
        resolved_path: &Path,
        parent_search: &[PathBuf],
    ) -> Result<LoadedModule> {
        let mut metadata = None;
        let mut defs = Vec::new();
        let mut deps = Vec::new();
        let module_dir = resolved_path.parent().unwrap_or(Path::new("."));

        // Build effective search paths: module's directory first, then parent search paths
        let mut module_search = vec![module_dir.to_path_buf()];
        module_search.extend_from_slice(parent_search);

        self.extract_filter(filter, &mut metadata, &mut defs, &mut deps, &module_search)?;

        Ok(LoadedModule {
            metadata,
            defs,
            deps,
            resolved_path: resolved_path.to_path_buf(),
        })
    }

    /// Recursively walk the filter AST to collect metadata, defs, and deps.
    #[allow(clippy::only_used_in_recursion)]
    fn extract_filter(
        &mut self,
        filter: &Filter,
        metadata: &mut Option<Value>,
        defs: &mut Vec<(String, Vec<String>, Filter)>,
        deps: &mut Vec<ModuleDep>,
        search_paths: &[PathBuf],
    ) -> Result<()> {
        match filter {
            Filter::ModuleDecl {
                metadata: meta,
                rest,
            } => {
                *metadata = Some(meta.clone());
                self.extract_filter(rest, metadata, defs, deps, search_paths)
            }
            Filter::Import {
                path,
                alias,
                is_data,
                metadata: import_meta,
                rest,
            } => {
                deps.push(ModuleDep {
                    relpath: path.clone(),
                    alias: Some(alias.clone()),
                    is_data: *is_data,
                    is_include: false,
                    search: import_meta.as_ref().and_then(extract_search),
                    metadata: import_meta.clone(),
                });
                self.extract_filter(rest, metadata, defs, deps, search_paths)
            }
            Filter::Include {
                path,
                metadata: inc_meta,
                rest,
            } => {
                deps.push(ModuleDep {
                    relpath: path.clone(),
                    alias: None,
                    is_data: false,
                    is_include: true,
                    search: inc_meta.as_ref().and_then(extract_search),
                    metadata: inc_meta.clone(),
                });
                self.extract_filter(rest, metadata, defs, deps, search_paths)
            }
            Filter::Def {
                name,
                params,
                body,
                rest,
            } => {
                defs.push((name.clone(), params.clone(), (**body).clone()));
                self.extract_filter(rest, metadata, defs, deps, search_paths)
            }
            _ => Ok(()),
        }
    }

    /// Bind a loaded module's definitions into an environment.
    /// If `namespace` is Some("foo"), defs are bound as "foo::name".
    /// If None (include), defs are bound directly by name.
    fn bind_module_defs(
        &mut self,
        module: &LoadedModule,
        namespace: Option<&str>,
        mut env: Env,
        parent_search: &[PathBuf],
    ) -> Result<Env> {
        let module_dir = module.resolved_path.parent().unwrap_or(Path::new("."));
        let mut module_search = vec![module_dir.to_path_buf()];
        module_search.extend_from_slice(parent_search);

        // First, resolve the module's own imports/includes to build its internal env
        let mut module_env = env.clone();
        for dep in &module.deps {
            let effective_search = if let Some(ref s) = dep.search {
                let mut paths = Vec::new();
                for base in &module_search {
                    paths.push(base.join(s));
                }
                paths
            } else {
                module_search.clone()
            };

            if dep.is_data {
                let resolved =
                    resolve_data_path(&dep.relpath, &effective_search, dep.search.as_deref())
                        .with_context(|| {
                            format!(
                                "resolving data dep '{}' from module {}",
                                dep.relpath,
                                module.resolved_path.display()
                            )
                        })?;
                let data = load_json_file(&resolved)?;
                if let Some(ref alias) = dep.alias {
                    module_env = module_env.bind_var(alias.clone(), data.clone());
                    let member = alias.trim_start_matches('$');
                    let ns_var = format!("{}::{}", alias, member);
                    module_env = module_env.bind_var(ns_var, data);
                }
            } else if dep.is_include {
                let dep_module = self
                    .load_module(&dep.relpath, &effective_search)
                    .with_context(|| {
                        format!(
                            "resolving include dep '{}' from module {}",
                            dep.relpath,
                            module.resolved_path.display()
                        )
                    })?;
                module_env =
                    self.bind_module_defs(&dep_module, None, module_env, &module_search)?;
            } else if let Some(ref alias) = dep.alias {
                let dep_module = self
                    .load_module(&dep.relpath, &effective_search)
                    .with_context(|| {
                        format!(
                            "resolving import dep '{}' as {} from module {}",
                            dep.relpath,
                            alias,
                            module.resolved_path.display()
                        )
                    })?;
                module_env =
                    self.bind_module_defs(&dep_module, Some(alias), module_env, &module_search)?;
            }
        }

        // Bind the module's own defs into module_env so later defs
        // can reference earlier defs in the same module.
        for (name, params, body) in &module.defs {
            let func = UserFunc {
                params: params.clone(),
                body: body.clone(),
                closure_env: module_env.clone(),
                is_def: true,
            };
            module_env = module_env.bind_func(name.clone(), params.len(), func);
        }

        // Now bind this module's defs into the target env
        for (name, params, body) in &module.defs {
            let func = UserFunc {
                params: params.clone(),
                body: body.clone(),
                closure_env: module_env.clone(),
                is_def: true,
            };
            let bound_name = if let Some(ns) = namespace {
                format!("{}::{}", ns, name)
            } else {
                name.clone()
            };
            env = env.bind_func(bound_name, params.len(), func);
        }

        Ok(env)
    }

    /// Get metadata for a module by name (for `modulemeta` builtin).
    pub fn get_module_metadata(&self, name: &str) -> Option<Value> {
        // Search cache for a module whose stem matches the name.
        // Prefer exact stem match (c.jq → "c") over parent-dir match (c/d.jq → "c").
        let mut best_match: Option<(&PathBuf, &LoadedModule)> = None;
        for (path, module) in &self.cache {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem == name {
                // Exact stem match — prefer this
                best_match = Some((path, module));
                break;
            }
            let parent_stem = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if parent_stem == name && best_match.is_none() {
                best_match = Some((path, module));
            }
        }
        if let Some((_path, module)) = best_match {
            // Build the modulemeta response
            let meta_obj = module
                .metadata
                .clone()
                .unwrap_or(Value::Object(Arc::new(vec![])));

            // Merge metadata fields with deps and defs
            let mut pairs = match meta_obj {
                Value::Object(ref p) => p.as_ref().clone(),
                _ => vec![],
            };

            // Add deps array — match jq's key order:
            // metadata fields (e.g., search) first, then as, is_data, relpath
            let deps_arr: Vec<Value> = module
                .deps
                .iter()
                .map(|d| {
                    let mut dep_pairs = vec![];
                    // Metadata fields first (e.g., "search")
                    if let Some(Value::Object(mp)) = &d.metadata {
                        for (k, v) in mp.iter() {
                            dep_pairs.push((k.clone(), v.clone()));
                        }
                    }
                    // "as" field — plain string, strip $ for data imports
                    if let Some(ref alias) = d.alias {
                        let as_name = alias.trim_start_matches('$');
                        dep_pairs.push(("as".to_string(), Value::String(as_name.to_string())));
                    }
                    dep_pairs.push(("is_data".to_string(), Value::Bool(d.is_data)));
                    dep_pairs.push(("relpath".to_string(), Value::String(d.relpath.clone())));
                    Value::Object(Arc::new(dep_pairs))
                })
                .collect();
            pairs.push(("deps".to_string(), Value::Array(Arc::new(deps_arr))));

            // Add defs array: ["name/arity", ...]
            let defs_arr: Vec<Value> = module
                .defs
                .iter()
                .map(|(name, params, _)| Value::String(format!("{}/{}", name, params.len())))
                .collect();
            pairs.push(("defs".to_string(), Value::Array(Arc::new(defs_arr))));

            return Some(Value::Object(Arc::new(pairs)));
        }
        None
    }

    /// Export all module metadata as a name→Value map for the eval thread-local cache.
    pub fn export_metadata(&self) -> HashMap<String, Value> {
        let mut result = HashMap::new();
        for path in self.cache.keys() {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let parent_stem = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("");
            // Use the more specific name (parent dir for nested modules)
            let name = if parent_stem != stem && !parent_stem.is_empty() {
                parent_stem
            } else {
                stem
            };
            if let Some(meta) = self.get_module_metadata(name) {
                result.insert(name.to_string(), meta);
            }
        }
        result
    }
}

/// Extract "search" field from module metadata.
fn extract_search(metadata: &Value) -> Option<String> {
    if let Value::Object(pairs) = metadata {
        for (k, v) in pairs.iter() {
            if k == "search"
                && let Value::String(s) = v
            {
                return Some(s.clone());
            }
        }
    }
    None
}

/// Resolve a module path (code module, .jq file).
/// Searches in order: `{search}/{path}.jq`, `{search}/{path}/{path}.jq`
fn resolve_module_path(path: &str, search_paths: &[PathBuf]) -> Result<PathBuf> {
    for base in search_paths {
        // Try {base}/{path}.jq
        let candidate = base.join(format!("{}.jq", path));
        if candidate.exists() {
            return Ok(candidate);
        }
        // Try {base}/{path}/{basename}.jq
        let basename = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        let candidate = base.join(path).join(format!("{}.jq", basename));
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!(
        "module not found: \"{path}\" (searched: {:?})",
        search_paths
    )
}

/// Resolve a data module path (.json file).
fn resolve_data_path(
    path: &str,
    search_paths: &[PathBuf],
    _extra_search: Option<&str>,
) -> Result<PathBuf> {
    for base in search_paths {
        let candidate = base.join(format!("{}.json", path));
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!(
        "data module not found: \"{path}\" (searched: {:?})",
        search_paths
    )
}

/// Load and parse a JSON file for data imports.
fn load_json_file(path: &Path) -> Result<Value> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read data module: {}", path.display()))?;
    let padded = crate::simdjson::pad_buffer(content.as_bytes());
    let val = crate::simdjson::dom_parse_to_value(&padded, content.len())
        .with_context(|| format!("failed to parse data module: {}", path.display()))?;
    // jq wraps data module values in an array
    Ok(Value::Array(Arc::new(vec![val])))
}
