# CLI Conformance TODOs

Remaining gaps between qj and jq CLI behavior, found during pre-release audit.

## Code fixes

### ~~1. Concatenated JSON documents on stdin~~ DONE
- Fixed: serde_json StreamDeserializer fallback when simdjson reports trailing content
- Also fixed: passthrough Identity validates single-doc before minifying
- Also fixed: CAPACITY error check used `contains()` which matched error code 15 as code 1

### ~~2. SIGPIPE handling~~ DONE
- Fixed: `libc::signal(SIGPIPE, SIG_DFL)` at program start

### Additional fixes found during implementation
- **Passthrough validation**: Identity passthrough now validates with `dom_parse_to_flat_buf_tape`
  before minifying, preventing incorrect output for multi-doc or invalid input
- **CAPACITY check bug**: `contains("simdjson error code 1")` matched codes 10-19; changed to `==`
- **--jsonargs exit code**: Invalid JSON in `--jsonargs` now exits 2 (matching jq) instead of 1

## Test-only additions

### ~~3. `$ENV` / `env` cross-tool conformance~~ DONE
### ~~4. `input`/`inputs` cross-tool conformance~~ DONE
### ~~5. Multiple valid files~~ DONE
### ~~6. `--arg` + `--args` combined~~ DONE
### ~~7. `--jsonargs` error handling~~ DONE
### ~~8. `--version` flag~~ DONE
### ~~9. `--indent 0` edge case~~ DONE
### ~~10. SIGPIPE test~~ DONE
### ~~11. Concatenated JSON test~~ DONE
