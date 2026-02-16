# Speed Up Single-Doc `reduce`

## Context

`reduce .[] as $x (0; .+1)` on 49MB `large_twitter.json` runs at 172ms (2.15x vs jq), far behind `length` at 34ms (10.6x) and `map({user, text})` at 94ms (3.9x). The bottleneck is **full Value tree materialization**: reduce is not flat-eval eligible, so it goes through `dom_parse_to_value_fast()` which builds the entire Value tree (~99ms) even though `reduce .[]` only iterates 2 top-level values. Meanwhile, `map` uses flat eval and skips that step entirely.

Arena allocation and per-thread output buffers won't help here. Arena doesn't change the fact that the entire tree is materialized. Per-thread output buffers are NDJSON-only.

## Changes

### 1. Fix flat eval reduce init (prerequisite)

**File:** `src/flat_eval.rs:371-377`

The flat eval Reduce handler materializes the full document to evaluate init:
```rust
let value = flat.to_value();  // materializes entire 49MB document!
eval_filter_with_env(init, &value, env, &mut |v| acc = v);
```

Replace with `eval_flat(init, flat, env, ...)` which handles literals directly (no materialization) and falls back to `flat.to_value()` only for exotic init expressions:
```rust
eval_flat(init, flat, env, &mut |v| acc = v);
```

### 2. Enable flat eval reduce for single-doc

**File:** `src/flat_eval.rs`, fn `is_flat_safe` (~line 158)

Add `Reduce` to the match. Only `source` needs to be flat-safe (it's passed to `eval_flat`). `init` also goes through `eval_flat` after change #1, but the catch-all handles arbitrary expressions. `update` runs via regular `eval_filter_with_env`, so it doesn't need the check:

```rust
Filter::Reduce(source, _pattern, init, _update) => {
    is_flat_safe(source) && is_flat_safe(init)
}
```

This routes single-doc reduce through flat eval in `process_padded` (`src/main.rs:866`), skipping the full `decode_value()` step.

### 3. Dead variable elimination — skip materialization when pattern var is unused

**File:** `src/flat_eval.rs` (Reduce handler) and `src/filter/mod.rs` (make `collect_pattern_var_refs` `pub(crate)`)

For `reduce .[] as $x (0; .+1)`, `$x` is never referenced in the update `.+1`. Currently, flat eval's Iterate handler calls `elem.to_value()` per element anyway. With dead-var elimination, we can count iterations without materializing:

Add a helper to check if the pattern variable is referenced in the update:
```rust
fn reduce_var_is_used(pattern: &Pattern, update: &Filter) -> bool {
    let mut pat_vars = HashSet::new();
    crate::filter::collect_pattern_var_refs(pattern, &mut pat_vars);
    let mut update_vars = HashSet::new();
    update.collect_var_refs(&mut update_vars);
    pat_vars.iter().any(|v| update_vars.contains(v))
}
```

Add a helper to count source elements from the flat buffer without materializing:
```rust
fn flat_source_count(filter: &Filter, flat: FlatValue<'_>, env: &Env) -> Option<usize> {
    match filter {
        Filter::Iterate => flat.len(),
        Filter::Pipe(left, right) => match eval_flat_nav(left, flat, env) {
            NavResult::Flat(child) => flat_source_count(right, child, env),
            _ => None,
        },
        _ => None,
    }
}
```

Then in the Reduce handler, branch on whether the pattern var is used:
- **Dead var + countable source:** Run update N times with `env` (no binding, no materialization)
- **Dead var + uncountable source:** Run `eval_flat(source, ...)` but skip `match_pattern` (still materializes per element, but saves HashMap clone)
- **Live var:** Current behavior (materialize + bind)

**Expected result:** `reduce .[] as $x (0; .+1)` drops from ~172ms to ~73ms (parse-bound), giving **~5x vs jq**.

### 4. Env scope chain (independent, broader impact)

**File:** `src/filter/mod.rs` (Env struct, `bind_var`, `get_var`)

Replace `Rc<HashMap<String, Value>>` with a linked scope chain. Every `bind_var` currently clones the entire HashMap. With a scope chain, binding is O(1) (one small allocation), lookup is O(depth) where depth is typically 1-5.

```rust
enum VarScope {
    Empty,
    Cons { name: String, value: Value, parent: Rc<VarScope> },
    Bulk(HashMap<String, Value>, Option<Rc<VarScope>>),
}
```

- `bind_var`: `Rc::new(VarScope::Cons { name, value, parent: self.vars.clone() })` — O(1)
- `get_var`: Walk chain until found — O(depth), depth typically < 5
- `Bulk` variant for initial env setup (`$ENV`, `--arg` bindings) so startup doesn't create N Cons nodes
- `is_empty`: Check for `Empty` variant

Benefits all variable-binding paths: reduce, foreach, `as` bindings, def parameters. Helps most when iteration count is high (e.g., `reduce .statuses[] as $x (...)`).

## Existing utilities to reuse

| Utility | Location | Purpose |
|---------|----------|---------|
| `Filter::collect_var_refs()` | `src/filter/mod.rs:415` | Collects all `$var` references in a filter |
| `collect_pattern_var_refs()` | `src/filter/mod.rs:527` | Collects variable names from a Pattern (needs `pub(crate)`) |
| `FlatValue::len()` | `src/flat_value.rs:122` | Returns element count for arrays/objects from flat buffer header |
| `eval_flat_nav()` | `src/flat_eval.rs:28` | Navigates filter on FlatValue without materializing |
| `eval_filter_with_env()` | `src/filter/eval.rs` | Regular eval with explicit env (used by flat eval Reduce for update) |

## Files to modify

- `src/flat_eval.rs` — Changes 1-3: fix init, add `is_flat_safe` for Reduce, dead-var optimization
- `src/filter/mod.rs` — Change 3: make `collect_pattern_var_refs` `pub(crate)`. Change 4: Env scope chain
- `src/filter/eval.rs` — Change 4 only: update `collect_pattern_vars` if needed (may be redundant after scope chain)

## Verification

```bash
# Unit + integration tests
cargo fmt && cargo clippy --release -- -D warnings && cargo test

# Benchmark reduce (target: ~73ms, currently ~172ms)
hyperfine --warmup 3 \
  './target/release/qj -c "reduce .[] as \$x (0; .+1)" benches/data/large_twitter.json' \
  'jq -c "reduce .[] as \$x (0; .+1)" benches/data/large_twitter.json'

# Benchmark reduce with used variable (should not regress)
hyperfine --warmup 3 \
  './target/release/qj -c "reduce .[] as \$x (0; . + 1)" benches/data/large_twitter.json'

# Correctness diff vs jq
diff <(./target/release/qj 'reduce .[] as $x (0; .+1)' benches/data/large_twitter.json) \
     <(jq 'reduce .[] as $x (0; .+1)' benches/data/large_twitter.json)

# Full compat suite
cargo test --release -- --ignored --nocapture
```
