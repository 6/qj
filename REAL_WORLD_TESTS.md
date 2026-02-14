# Real-world jq compatibility

> The feature matrix below is also available as an executable test suite:
> `bash tests/jq_compat/run_features.sh`

The jq.test conformance suite (497 tests) only exercises filter expressions piped through
`printf '%s' "$input" | tool -c -- "$filter"`. It tests zero CLI flags, zero error handling,
zero real JSON shapes, and zero pipeline behavior. A tool could hit 100% on jq.test while
failing basic real-world workflows.

This document catalogs every jq feature with support status across tools, then provides
real-world test cases and performance expectations.

**Current conformance:** jx 42%, jaq 69%, gojq 86%, jq 100% (497 filter-expression tests)

---

## Feature compatibility matrix

Status: **Y** = works, **N** = not implemented, **~** = partial/buggy

### Types and basic filters

| Feature | Example | jx | jaq | gojq |
|---------|---------|:--:|:---:|:----:|
| Identity | `.` | Y | Y | Y |
| Field access | `.foo`, `.["key"]` | Y | Y | Y |
| Optional field | `.foo?` | Y | Y | Y |
| Array index | `.[0]`, `.[-1]` | Y | Y | Y |
| Negative index (out-of-range) | `[-1] \| .[-2]` → `null` | ~ | ~ | ~ |
| Array/string slice | `.[2:5]`, `.[3:]`, `.[:2]` | Y | Y | Y |
| Iterator | `.[]`, `.[]?` | Y | Y | Y |
| Recursive descent | `..` | Y | Y | Y |
| Type literals | `null`, `true`, `false`, `42`, `"str"` | Y | Y | Y |
| `type` | `42 \| type` → `"number"` | Y | Y | Y |

### Operators

| Operator | Example | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `+` (num/str/arr/obj) | `1 + 2`, `"a" + "b"`, `[1]+[2]`, `{a:1}+{b:2}` | Y | Y | Y |
| `-` (num/arr) | `5 - 3`, `[1,2,3] - [2]` | Y | Y | Y |
| `*` (num/str repeat/obj merge) | `2 * 3`, `"ab" * 3`, `{a:{b:1}} * {a:{c:2}}` | Y | Y | Y |
| `/` (num/str split) | `10 / 3`, `"a,b" / ","` | Y | Y | Y |
| `%` (modulo) | `17 % 5` → `2` | Y | Y | Y |
| `==`, `!=` | `.x == 1` | Y | Y | Y |
| `<`, `<=`, `>`, `>=` | `.x > 5` | Y | Y | Y |
| `and`, `or` | `.a and .b` | Y | Y | Y |
| `not` | `true \| not` → `false` | Y | Y | Y |
| `//` (alternative) | `.x // "default"` | Y | Y | Y |
| `\|` (pipe) | `.a \| .b` | Y | Y | Y |
| `,` (comma) | `.a, .b` | Y | Y | Y |
| Unary `-` | `-(. + 1)` | Y | Y | Y |

### Control flow

| Feature | Example | jx | jaq | gojq |
|---------|---------|:--:|:---:|:----:|
| `if-then-else-end` | `if .x > 0 then "pos" else "neg" end` | Y | Y | Y |
| `elif` chains | `if . == 1 then "a" elif . == 2 then "b" else "c" end` | Y | Y | Y |
| `try` (error suppression) | `.foo?`, `try .foo` | Y | Y | Y |
| `try-catch` | `try error catch .` | Y | Y | Y |
| `expr as $var \| body` | `.name as $n \| {name: $n}` | Y | Y | Y |
| `$var` reference | `1 as $x \| $x + $x` | Y | Y | Y |
| `reduce` | `reduce .[] as $x (0; . + $x)` | N | Y | Y |
| `foreach` | `foreach .[] as $x (0; . + $x)` | N | Y | Y |
| `def name: body;` | `def double: . * 2; 5 \| double` | N | Y | Y |
| `def name(args): body;` | `def f(x): x \| x; [1,2] \| f(.[])` | N | Y | Y |
| `label $name \| break $name` | `label $out \| foreach .[] as $x (0; .+$x; if .>3 then ., break $out else . end)` | N | N | Y |
| `empty` | `empty` (no output) | Y | Y | Y |
| `error` / `error(msg)` | `if .x == null then error("missing x") end` | ~ | Y | Y |

### Array functions

| Function | Example | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `length` | `[1,2,3] \| length` → `3` | Y | Y | Y |
| `reverse` | `[1,2,3] \| reverse` → `[3,2,1]` | Y | Y | Y |
| `sort` | `[3,1,2] \| sort` → `[1,2,3]` | Y | Y | Y |
| `sort_by(f)` | `[{a:2},{a:1}] \| sort_by(.a)` | Y | Y | Y |
| `group_by(f)` | `[{a:1},{a:2},{a:1}] \| group_by(.a)` | Y | Y | Y |
| `unique` | `[1,2,1] \| unique` → `[1,2]` | Y | Y | Y |
| `unique_by(f)` | `[{a:1,b:2},{a:1,b:3}] \| unique_by(.a)` | Y | Y | Y |
| `min` / `max` | `[3,1,2] \| min` → `1` | Y | Y | Y |
| `min_by(f)` / `max_by(f)` | `[{a:2},{a:1}] \| min_by(.a)` | Y | Y | Y |
| `add` | `[1,2,3] \| add` → `6` | Y | Y | Y |
| `flatten` / `flatten(n)` | `[[1,[2]],3] \| flatten` → `[1,2,3]` | Y | Y | Y |
| `transpose` | `[[1,2],[3,4]] \| transpose` → `[[1,3],[2,4]]` | Y | Y | Y |
| `contains(x)` | `[1,2,3] \| contains([2])` → `true` | Y | Y | Y |
| `inside(x)` | `[2] \| inside([1,2,3])` → `true` | Y | Y | Y |
| `index(x)` / `rindex(x)` | `"abcabc" \| index("bc")` → `1` | Y | Y | Y |
| `indices(x)` | `"abcabc" \| indices("bc")` → `[1,4]` | Y | Y | Y |
| `first` / `last` | `[1,2,3] \| first` → `1` | Y | Y | Y |
| `first(f)` / `last(f)` | `first(range(10))` → `0` | Y | Y | Y |
| `nth(n; f)` | `nth(2; range(10))` → `2` | Y | Y | Y |
| `range(n)` | `[range(3)]` → `[0,1,2]` | Y | Y | Y |
| `range(from;to)` | `[range(2;5)]` → `[2,3,4]` | Y | Y | Y |
| `range(from;to;by)` | `[range(0;10;3)]` → `[0,3,6,9]` | Y | Y | Y |
| `any` / `any(f)` | `[1,2,3] \| any(. > 2)` → `true` | Y | Y | Y |
| `all` / `all(f)` | `[1,2,3] \| all(. > 0)` → `true` | Y | Y | Y |
| `map(f)` | `[1,2,3] \| map(. + 10)` → `[11,12,13]` | Y | Y | Y |
| `map_values(f)` | `{a:1,b:2} \| map_values(.+10)` | Y | Y | Y |
| `select(f)` | `.[] \| select(. > 2)` | Y | Y | Y |
| `limit(n; f)` | `[limit(3; range(100))]` → `[0,1,2]` | Y | Y | Y |
| `until(cond; update)` | `0 \| until(. >= 5; . + 1)` → `5` | Y | Y | Y |
| `while(cond; update)` | `[1 \| while(. < 8; . * 2)]` → `[1,2,4]` | Y | Y | Y |
| `repeat(f)` | `1 \| [limit(3; repeat(. * 2))]` | Y | Y | Y |
| `recurse` / `recurse(f)` / `recurse(f;cond)` | `2 \| recurse(. * .; . < 100)` | Y | Y | Y |
| `isempty(f)` | `isempty(empty)` → `true` | Y | Y | Y |
| `walk(f)` | `[1,[2]] \| walk(if type=="array" then sort else . end)` | N | ~ | Y |
| `combinations` / `combinations(n)` | `[[1,2],[3,4]] \| combinations` | N | Y | Y |
| `bsearch(x)` | `[1,2,3] \| bsearch(2)` → `1` | Y | Y | Y |
| `pick(paths)` | `{a:1,b:{c:2}} \| pick(.a, .b.c)` | N | N | Y |

### Object functions

| Function | Example | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `keys` | `{b:2,a:1} \| keys` → `["a","b"]` | Y | Y | Y |
| `keys_unsorted` | `{b:2,a:1} \| keys_unsorted` → `["b","a"]` | Y | Y | Y |
| `values` (iterate) | `{a:1,b:2} \| [values]` → `[1,2]` | Y | Y | Y |
| `values` (type selector) | `[1,null,2] \| [.[] \| values]` → `[1,2]` | N | Y | Y |
| `has(key)` | `{a:1} \| has("a")` → `true` | Y | Y | Y |
| `in(obj)` | `"a" \| in({a:1})` | N | Y | Y |
| `to_entries` | `{a:1} \| to_entries` → `[{"key":"a","value":1}]` | Y | Y | Y |
| `from_entries` | `[{"key":"a","value":1}] \| from_entries` → `{"a":1}` | Y | Y | Y |
| `with_entries(f)` | `{a:1,b:2} \| with_entries(select(.value > 1))` | Y | Y | Y |
| `del(path)` | `{a:1,b:2} \| del(.a)` → `{"b":2}` | ~ | Y | Y |
| `paths` / `paths(f)` | `{a:{b:1}} \| [paths]` → `[["a"],["a","b"]]` | Y | ~ | Y |
| `path(expr)` | `{a:{b:1}} \| path(.a.b)` → `["a","b"]` | Y | N | Y |
| `leaf_paths` | `{a:{b:1},c:2} \| [leaf_paths]` | Y | N | Y |
| `getpath(p)` | `{a:{b:1}} \| getpath(["a","b"])` → `1` | Y | Y | Y |
| `setpath(p; v)` | `{} \| setpath(["a"]; 1)` → `{"a":1}` | Y | Y | Y |
| `delpaths(ps)` | `{a:1,b:2} \| delpaths([["a"]])` → `{"b":2}` | Y | Y | Y |

Notes on `del`: jx only supports `del(.field)` on objects, not arbitrary path expressions
like `del(.[0])` or `del(.a.b)`.

### String functions

| Function | Example | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `tostring` | `42 \| tostring` → `"42"` | Y | Y | Y |
| `tonumber` | `"42" \| tonumber` → `42` | Y | Y | Y |
| `split(s)` | `"a,b,c" \| split(",")` → `["a","b","c"]` | Y | Y | Y |
| `split("")` | `"abc" \| split("")` → `["a","b","c"]` | Y | Y | Y |
| `join(s)` | `["a","b"] \| join(",")` → `"a,b"` | Y | Y | Y |
| `ltrimstr(s)` / `rtrimstr(s)` | `"hello.txt" \| rtrimstr(".txt")` → `"hello"` | Y | Y | Y |
| `startswith(s)` / `endswith(s)` | `"hello" \| startswith("he")` → `true` | Y | Y | Y |
| `ascii_upcase` / `ascii_downcase` | `"Hello" \| ascii_downcase` → `"hello"` | Y | Y | Y |
| `trim` / `ltrim` / `rtrim` | `"  hi  " \| trim` → `"hi"` | Y | N | N |
| `explode` / `implode` | `"abc" \| explode` → `[97,98,99]` | Y | Y | Y |
| `tojson` / `fromjson` | `[1,2] \| tojson` → `"[1,2]"` | Y | Y | Y |
| `utf8bytelength` | `"e\u0301" \| utf8bytelength` → `3` | Y | Y | Y |
| `ascii` | `"A" \| ascii` → `65` | Y | N | N |
| `"\(expr)"` interpolation | `"name: \(.name)"` | Y | Y | Y |
| `test(re)` / `test(re; flags)` | `"foo" \| test("^f")` → `true` | N | Y | Y |
| `match(re)` / `match(re; flags)` | `"foo" \| match("(o+)")` | N | Y | Y |
| `capture(re)` | `"2024-01-15" \| capture("(?<y>\\d+)-(?<m>\\d+)")` | N | Y | Y |
| `sub(re; repl)` / `gsub(re; repl)` | `"foo" \| gsub("o"; "0")` → `"f00"` | N | Y | Y |
| `scan(re)` | `"test 123 test 456" \| [scan("[0-9]+")]` | N | Y | Y |
| `splits(re)` | `"a1b2c" \| [splits("[0-9]+")]` | N | Y | Y |

### Math functions

| Function | Example | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `floor` / `ceil` / `round` / `trunc` | `3.7 \| floor` → `3` | Y | Y | Y |
| `sqrt` / `cbrt` | `9 \| sqrt` → `3.0` | Y | Y | Y |
| `abs` / `fabs` | `-5 \| abs` → `5` | Y | Y | Y |
| `exp` / `exp2` | `1 \| exp` → `2.718...` | Y | Y | Y |
| `log` / `log2` / `log10` | `100 \| log10` → `2` | Y | Y | Y |
| `logb` / `significand` / `exponent` | `8 \| logb` → `3` | Y | N | Y |
| `pow(x;y)` | `pow(2;10)` → `1024` | Y | Y | Y |
| `sin`/`cos`/`tan`/`asin`/`acos`/`atan` | `0 \| cos` → `1` | Y | Y | Y |
| `atan2(y;x)` | `atan2(1;0)` → `1.5707...` | Y | Y | Y |
| `sinh`/`cosh`/`tanh`/`asinh`/`acosh`/`atanh` | `0 \| sinh` → `0` | Y | Y | Y |
| `nan` / `infinite` / `inf` | `nan \| isnan` → `true` | Y | Y | Y |
| `isnan`/`isinfinite`/`isfinite`/`isnormal` | `1 \| isfinite` → `true` | Y | Y | Y |
| `nearbyint` / `rint` | `3.7 \| nearbyint` → `4` | Y | N | Y |
| `j0` / `j1` (Bessel) | `0 \| j0` → `1` | Y | N | Y |
| `scalb(x;e)` | `2 \| scalb(3)` → `16` | Y | N | Y |
| `remainder(x;y)` / `hypot(x;y)` | `hypot(3;4)` → `5` | Y | Y | Y |
| `fma(x;y;z)` | `fma(2;3;4)` → `10` | Y | N | Y |

### Type selectors

| Selector | Meaning | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `arrays` | select arrays | Y | Y | Y |
| `objects` | select objects | Y | Y | Y |
| `numbers` | select numbers | Y | Y | Y |
| `strings` | select strings | Y | Y | Y |
| `booleans` | select booleans | Y | Y | Y |
| `nulls` | select nulls | Y | Y | Y |
| `values` | select non-null | N | Y | Y |
| `scalars` | select non-iterable | Y | Y | Y |
| `iterables` | select arrays/objects | Y | Y | Y |

### Format strings

| Format | Purpose | jx | jaq | gojq |
|--------|---------|:--:|:---:|:----:|
| `@base64` / `@base64d` | Base64 encode/decode | N | Y | Y |
| `@uri` | URL percent-encoding | N | Y | Y |
| `@csv` | CSV formatting | N | Y | Y |
| `@tsv` | TSV formatting | N | Y | Y |
| `@html` | HTML entity escaping | N | Y | Y |
| `@sh` | Shell escaping | N | Y | Y |
| `@json` / `@text` | JSON/text serialization | N | Y | Y |

### Date/time

| Function | Example | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `now` | `now` → `1707849600.0` | Y | Y | Y |
| `todate` | `0 \| todate` → `"1970-01-01T00:00:00Z"` | Y | Y | Y |
| `fromdate` | `"1970-01-01T00:00:00Z" \| fromdate` → `0` | Y | Y | Y |
| `strftime(fmt)` | `0 \| strftime("%Y-%m-%d")` → `"1970-01-01"` | Y | Y | Y |
| `strptime(fmt)` | `"2024-01-15" \| strptime("%Y-%m-%d")` | N | N | Y |
| `gmtime` / `mktime` | `0 \| gmtime` → `[0,0,0,1,0,70,4,0]` | N | N | Y |

### Assignment operators

| Operator | Example | jx | jaq | gojq |
|----------|---------|:--:|:---:|:----:|
| `\|=` (update) | `.a \|= . + 1` | N | Y | Y |
| `+=`, `-=`, `*=`, `/=`, `%=` | `.a += 1` | N | Y | Y |
| `//=` (alternative assign) | `.a //= "default"` | N | Y | Y |
| `=` (plain assign) | `.a = 1` | N | Y | Y |

### I/O and environment

| Feature | Example | jx | jaq | gojq |
|---------|---------|:--:|:---:|:----:|
| `env` / `$ENV` | `env.HOME` | Y | Y | Y |
| `debug` / `debug(label)` | `42 \| debug("val")` | Y | Y | Y |
| `error` / `error(msg)` | `error("fail")` | ~ | Y | Y |
| `input` / `inputs` | `[inputs]` (read all) | N | Y | Y |
| `halt` / `halt_error(code)` | `halt_error(1)` | N | N | Y |
| `builtins` | `builtins \| length` | Y | Y | Y |

### Streaming

| Feature | Example | jx | jaq | gojq |
|---------|---------|:--:|:---:|:----:|
| `--stream` flag | `jq --stream '.'` → path-value pairs | N | N | Y |
| `tostream` / `fromstream` | `{a:1} \| [tostream]` | N | N | Y |
| `truncate_stream(f)` | | N | N | Y |

### SQL-style operators

| Feature | Example | jx | jaq | gojq |
|---------|---------|:--:|:---:|:----:|
| `IN(generator)` | `3 \| IN(1, 2, 3)` → `true` | Y | Y | Y |
| `IN(stream; generator)` | `.[] \| IN(.; 1, 2, 3)` | Y | Y | Y |
| `INDEX(stream; expr)` | `INDEX(.[], .name)` | N | Y | Y |
| `GROUP_BY(expr)` | `GROUP_BY(.a)` | N | Y | Y |

### CLI flags

| Flag | Purpose | jx | jaq | gojq |
|------|---------|:--:|:---:|:----:|
| `-c` / `--compact-output` | One-line output | Y | Y | Y |
| `-r` / `--raw-output` | Unquoted strings | Y | Y | Y |
| `-n` / `--null-input` | Use `null` as input | Y | Y | Y |
| `-e` / `--exit-status` | Exit code on false/null | Y | Y | Y |
| `--tab` | Tab indentation | Y | Y | Y |
| `--indent N` | Custom indentation | Y | Y | Y |
| `--jsonl` | Force NDJSON mode | Y | N | N |
| `-s` / `--slurp` | Slurp all inputs to array | N | Y | Y |
| `-S` / `--sort-keys` | Sort object keys | N | Y | Y |
| `-R` / `--raw-input` | Read lines as strings | N | Y | Y |
| `-j` / `--join-output` | No trailing newline | N | Y | Y |
| `--arg name val` | Bind string variable | N | Y | Y |
| `--argjson name val` | Bind JSON variable | N | Y | Y |
| `-f` / `--from-file` | Read filter from file | N | Y | Y |
| `--slurpfile var file` | Load JSON array var | N | Y | Y |
| `-C` / `--color-output` | Force color | N | Y | Y |
| `-M` / `--monochrome` | Disable color | N | Y | Y |

### jx-only features

| Feature | Description |
|---------|-------------|
| SIMD parsing | simdjson On-Demand for 7-9 GB/s parse throughput |
| Parallel NDJSON | Automatic rayon chunked processing with order preservation |
| Passthrough fast paths | Identity compact, field length, field keys bypass evaluator |
| `--jsonl` flag | Explicit NDJSON mode (auto-detected without flag) |
| `--debug-timing` | Per-phase timing breakdown for profiling |

---

## Real-world test cases

### Kubernetes (`.items[]` wrapper, deep nesting)

```json
{"items":[
  {"metadata":{"name":"web-1","namespace":"prod","labels":{"app":"web"}},
   "spec":{"containers":[{"name":"nginx","image":"nginx:1.25","ports":[{"containerPort":80}]}]},
   "status":{"phase":"Running"}},
  {"metadata":{"name":"api-1","namespace":"prod","labels":{"app":"api"}},
   "spec":{"containers":[{"name":"app","image":"myapp:v2","ports":[{"containerPort":8080}]}]},
   "status":{"phase":"Pending"}}
]}
```

| # | Filter | Expected | Features exercised |
|---|--------|----------|--------------------|
| 1 | `.items[] \| .metadata.name` | `"web-1"` `"api-1"` | iterate, nested field |
| 2 | `.items[] \| select(.status.phase == "Running") \| .metadata.name` | `"web-1"` | select, compare, field |
| 3 | `.items[] \| {name: .metadata.name, image: .spec.containers[0].image}` | `{"name":"web-1","image":"nginx:1.25"}` ... | object construct, index |
| 4 | `.items \| length` | `2` | field, length |
| 5 | `[.items[] \| .metadata.labels.app] \| unique` | `["api","web"]` | array construct, unique |
| 6 | `.items[] \| .spec.containers[] \| .ports[] \| .containerPort` | `80` `8080` | multi-level iterate |

### GitHub API (array of objects, nullable fields)

```json
[
  {"number":1,"title":"Add feature","state":"open","user":{"login":"alice"},"draft":false,"labels":[{"name":"enhancement"}]},
  {"number":2,"title":"Fix bug","state":"closed","user":{"login":"bob"},"draft":false,"labels":[]},
  {"number":3,"title":"WIP","state":"open","user":{"login":"alice"},"draft":true,"labels":[{"name":"wip"}]}
]
```

| # | Filter | Expected | Features exercised |
|---|--------|----------|--------------------|
| 1 | `.[] \| {number, title, state}` | 3 objects | shorthand object construct |
| 2 | `[.[] \| select(.state == "open" and .draft == false)]` | `[{number:1,...}]` | select, boolean and |
| 3 | `[.[] \| .user.login] \| unique` | `["alice","bob"]` | nested field, unique |
| 4 | `group_by(.user.login) \| map({user: .[0].user.login, count: length})` | grouped counts | group_by, map, length |
| 5 | `.[] \| .labels[] \| .name` | `"enhancement"` `"wip"` | nested iterate |
| 6 | `.[] \| select(.labels \| length > 0) \| .number` | `1` `3` | select with piped condition |

### NDJSON structured logs (one object per line)

```
{"ts":"2024-01-15T10:00:01Z","level":"info","msg":"request started","method":"GET","path":"/api/users"}
{"ts":"2024-01-15T10:00:02Z","level":"error","msg":"database timeout","err":{"code":"ETIMEDOUT","stack":"Error: ETIMEDOUT\n    at Pool.query..."}}
{"ts":"2024-01-15T10:00:03Z","level":"info","msg":"request completed","method":"GET","path":"/api/users","status":200,"duration_ms":1523}
```

| # | Filter | Expected | Features exercised |
|---|--------|----------|--------------------|
| 1 | `select(.level == "error") \| .msg` | `"database timeout"` | NDJSON, select, field |
| 2 | `select(.level == "error") \| .err.code` | `"ETIMEDOUT"` | nested field on error |
| 3 | `select(.duration_ms != null) \| {path, duration_ms}` | filtered objects | null check, construct |
| 4 | `.ts` | all 3 timestamps | field extraction at scale |
| 5 | `select(.status >= 400 // false)` | (none match) | alternative with select |

### AWS CloudTrail (`{Records:[...]}` wrapper)

```json
{"Records":[
  {"eventTime":"2024-01-15T10:00:00Z","eventSource":"iam.amazonaws.com","eventName":"CreateUser","userIdentity":{"arn":"arn:aws:iam::123:user/admin"}},
  {"eventTime":"2024-01-15T10:05:00Z","eventSource":"s3.amazonaws.com","eventName":"PutObject","userIdentity":{"arn":"arn:aws:iam::123:role/deploy"}},
  {"eventTime":"2024-01-15T10:10:00Z","eventSource":"iam.amazonaws.com","eventName":"DeleteUser","userIdentity":{"arn":"arn:aws:iam::123:user/admin"}}
]}
```

| # | Filter | Expected | Features exercised |
|---|--------|----------|--------------------|
| 1 | `.Records[] \| select(.eventSource == "iam.amazonaws.com") \| .eventName` | `"CreateUser"` `"DeleteUser"` | field, select, iterate |
| 2 | `.Records[] \| {time: .eventTime, action: .eventName, who: .userIdentity.arn}` | 3 reshaped objects | object construct, rename |
| 3 | `[.Records[] \| .eventSource] \| unique` | `["iam.amazonaws.com","s3.amazonaws.com"]` | unique |
| 4 | `.Records \| length` | `3` | length |
| 5 | `.Records[] \| select(.eventName \| startswith("Delete"))` | 1 event | startswith |

### Docker inspect (single deep object)

```json
[{"Id":"abc123def","State":{"Status":"running","Pid":12345},
  "Config":{"Image":"nginx:latest","Env":["PORT=8080","NODE_ENV=production"]},
  "NetworkSettings":{"Networks":{"bridge":{"IPAddress":"172.17.0.2"}}}}]
```

| # | Filter | Expected | Features exercised |
|---|--------|----------|--------------------|
| 1 | `.[0].State.Status` | `"running"` | index, nested field |
| 2 | `.[0].Config.Env[] \| split("=") \| {key: .[0], value: .[1]}` | 2 kv objects | iterate, split, construct |
| 3 | `.[0].NetworkSettings.Networks \| keys` | `["bridge"]` | deeply nested keys |
| 4 | `.[0].Id[:12]` | `"abc123def"` (truncated) | string slice |

### npm package-lock.json (large, repetitive)

```json
{"name":"my-app","version":"1.0.0",
 "packages":{"":{"name":"my-app","version":"1.0.0"},
  "node_modules/express":{"version":"4.18.2","resolved":"https://registry.npmjs.org/express/-/express-4.18.2.tgz"},
  "node_modules/lodash":{"version":"4.17.21","resolved":"https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz"}}}
```

| # | Filter | Expected | Features exercised |
|---|--------|----------|--------------------|
| 1 | `.packages \| keys \| length` | `3` | keys, length |
| 2 | `.packages \| to_entries[] \| select(.key \| startswith("node_modules/")) \| {pkg: (.key \| ltrimstr("node_modules/")), version: .value.version}` | 2 pkg objects | to_entries, select, ltrimstr |
| 3 | `[.packages \| to_entries[] \| .value.version] \| unique \| sort` | sorted unique versions | unique, sort |

---

## CLI flag and pipeline tests

### Flag combinations by frequency

| Combo | Use case | jx | Example |
|-------|----------|:--:|---------|
| `-r '.field'` | Shell variable assignment | Y | `name=$(echo '{"n":"x"}' \| jx -r '.n')` |
| `-c '.'` | Compact for piping | Y | `jx -c '.' big.json \| wc -c` |
| `-c '.[]'` | Compact NDJSON output | Y | `jx -c '.items[]' k8s.json` |
| `-n 'expr'` | Generate from nothing | Y | `jx -n '{a:1, b:2}'` |
| `-e 'select(...)'` | Conditional in shell | Y | `if jx -e 'select(.ok)' r.json; then ...` |
| `-s '.'` | Slurp NDJSON to array | N | `cat *.json \| jx -s '.'` |
| `-r -s 'sort \| .[]'` | Sort NDJSON lines | N | `cat log.jsonl \| jx -r -s 'sort_by(.ts) \| .[] \| .msg'` |
| `--arg name val` | Parameterized filter | N | `jx --arg user "$USER" '.[] \| select(.name == $user)'` |
| `--argjson n 5` | Numeric parameter | N | `jx --argjson n 5 '.[:$n]'` |
| `-S -c '.'` | Sorted keys for diff | N | `diff <(jx -Sc . a.json) <(jx -Sc . b.json)` |
| `-R '.'` | Process non-JSON lines | N | `cat file.txt \| jx -R '.'` |
| `-rj '.msg'` | No trailing newline | N | `jx -rj '.msg' event.json` |

### Pipeline patterns

```bash
# API response processing
curl -s https://api.github.com/repos/jqlang/jq/releases | jx '.[0].tag_name'

# Kubernetes
kubectl get pods -o json | jx -r '.items[] | select(.status.phase != "Running") | .metadata.name'

# Log processing (jx NDJSON strength)
cat app.log.json | jx -r 'select(.level == "error") | .msg'

# NDJSON field extraction pipeline
cat events.jsonl | jx -r '.user_id' | sort | uniq -c | sort -rn

# Preprocessor: jx for speed, jq for complex filter
jx -c '.items[]' huge_k8s.json | jq 'select(.spec.containers | length > 1)'

# Multi-file (sequential)
jx '.name' a.json b.json c.json

# Generate + consume
jx -n '[range(100)] | map({id: ., active: (. % 2 == 0)})' | jx '[.[] | select(.active)] | length'
```

---

## Error behavior

| Scenario | jq behavior | What to test |
|----------|-------------|-------------|
| Malformed JSON input | stderr error, exit 2 | Same exit code, no stdout |
| Truncated JSON `{"a":` | stderr error, exit 2 | Same |
| Empty stdin | No output, exit 0 (exit 4 with `-e`) | Match jq |
| Missing field `.x` on `{"y":1}` | `null` | Already works |
| `.[]` on string | Error to stderr | Exit code, error to stderr |
| Division by zero `1/0` | Error | Match behavior |
| Unknown builtin `foobar` | Error | Error to stderr (jx silently produces nothing) |
| NDJSON with malformed lines | Valid lines produce output, errors on stderr | Order preserved, partial output |

---

## Performance expectations

Based on existing benchmarks (Apple Silicon, bench.sh results).

| Workload | File size | jx vs jq | jx vs jaq | Notes |
|----------|-----------|:--------:|:---------:|-------|
| Identity compact | 49 MB | 63x | 14x | SIMD passthrough (`simdjson::minify`) |
| Field access | 49 MB | 15x | 3.3x | DOM parse fast path |
| `.field \| length` | 49 MB | 12x | 5.1x | C++ passthrough |
| `.field \| keys` | 49 MB | 13x | 5.3x | C++ passthrough |
| `.field[] \| .nested` | 49 MB | 2.5x | ~1x | Eval-dominated, parse advantage offset |
| select + construct | 631 KB | 2x | ~1x | Small file, eval parity |
| NDJSON field extract | 82 MB (1M lines) | 10x | 5.6x | Parallel processing |
| Small file any filter | <1 MB | 2-3x | ~1x | Startup cost dominates |

### Performance by data shape (expected)

| Data shape | Identity/extract | Iterate+filter | Aggregate | Why |
|-----------|:---------------:|:-------------:|:---------:|-----|
| K8s pod list (100KB-1MB) | 3-5x vs jq | ~2x | ~2x | Moderate size, deep nesting |
| Package-lock.json (10-50MB) | 15-60x vs jq | 3-10x | needs --slurp | Very large, regular structure, SIMD shines |
| NDJSON logs (1M+ lines) | 10x+ vs jq | 5-10x | needs --slurp | Parallel processing |
| GitHub API response (<500KB) | 2-3x vs jq | ~1x | ~1x | Small, startup-dominated |
| CloudTrail (1-10MB) | 5-15x vs jq | 2-5x | needs --slurp | Moderate size, array wrapper |
| Docker inspect (<50KB) | ~2x vs jq | ~1x | N/A | Tiny, no SIMD advantage |

### What blocks real-world performance testing

1. **`--slurp` not implemented** — can't benchmark aggregation across NDJSON (sort, group_by, reduce)
2. **Only Twitter API test data** — bench.sh uses one JSON shape; need k8s, log, lock file variants
3. **No large NDJSON with realistic shapes** — current gen_ndjson produces 7 flat fields; real logs have nested errors, variable-length messages

---

## Feature summary

| Category | Implemented | Total | Coverage |
|----------|:-----------:|:-----:|:--------:|
| Types & basic filters | 10 | 10 | 100% |
| Operators | 14 | 14 | 100% |
| Control flow | 9 | 13 | 69% |
| Array functions | 29 | 32 | 91% |
| Object functions | 13 | 16 | 81% |
| String functions | 14 | 20 | 70% |
| Math functions | 16 | 16 | 100% |
| Type selectors | 8 | 9 | 89% |
| Format strings | 0 | 8 | 0% |
| Date/time | 4 | 6 | 67% |
| Assignment operators | 0 | 4 | 0% |
| I/O & environment | 3 | 6 | 50% |
| Streaming | 0 | 3 | 0% |
| SQL-style | 2 | 4 | 50% |
| CLI flags | 7 | 17 | 41% |
| **Total** | **129** | **178** | **72%** |

### What's missing that matters most

Ranked by impact on real-world adoption:

1. **`--slurp`** — blocks any workflow that aggregates across NDJSON lines or multiple files
2. **`--arg` / `--argjson`** — blocks any parameterized script
3. **`reduce`** — blocks accumulation patterns (`reduce .[] as $x (0; . + $x)`)
4. **Assignment (`|=`, `+=`)** — blocks in-place modification patterns
5. **Regex (`test`, `match`, `gsub`)** — blocks string pattern matching
6. **Format strings (`@base64`, `@csv`, `@tsv`)** — blocks output formatting
7. **`def`** — blocks user-defined functions and complex reusable filters
8. **`--sort-keys`** — blocks diffable output workflows
9. **`walk(f)`** — blocks recursive transformation patterns
10. **`in(obj)`** — blocks key existence checks with `in` syntax
