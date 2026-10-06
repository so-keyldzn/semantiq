---
name: semantiq
description: Code intelligence for the current project through the `semantiq` CLI (run via Bash), answered from a syntax-aware index rather than text matches. Use it for structural questions that grep answers badly - who calls this function and what does it call, what implements this trait / interface or extends this class, where is this symbol really used (not in comments or strings), what breaks before renaming or changing a function or type and which tests to run, which code is unused / dead, what a file imports and who imports it - and for conceptual questions with no keyword to grep ("where is X handled?", "how does Y work?"), or a ranked map of an unfamiliar repository. Do NOT use it for exact strings, regexes, error messages or config keys - grep / rg is faster and exhaustive there. Requires a `.semantiq.db` index (`semantiq index` or `semantiq init`).
---

# Semantiq CLI

`semantiq` answers questions about the code from a local index (`.semantiq.db`,
found in the current directory or a parent). Each command refreshes files
changed since the last run, then prints compact results on stdout: one line
per result, each path written once. Add `--json` to get structured output
(same schema as the MCP tools), ideal for `jq` and chaining.

## Pick the command

| You need… | Run |
|---|---|
| An overview of an unfamiliar repo | `semantiq map` (`--focus <file,dir,symbol>`) |
| Code for a concept, without knowing names | `semantiq search "<what the code does>"` |
| A symbol whose name you only half know | `semantiq search "<name fragment>" --symbol-kind function` |
| Every definition and usage of a symbol | `semantiq refs <symbol>` |
| Who calls a function / what it calls | `semantiq calls <symbol> --direction callers\|callees` |
| What a type implements / what implements it | `semantiq hierarchy <type>` |
| What breaks if a symbol changes + tests to run | `semantiq impact <symbol>` |
| Unused functions, methods and types | `semantiq dead-code --path-prefix src/` |
| Signature, docs, usage count of a symbol | `semantiq explain <symbol>` |
| What a file imports / which files import it | `semantiq deps <path/from/project/root>` |
| An exact string, regex, error text, config key | `grep -rn` / `rg` (not semantiq) |

Typical flow before a change: `search` (or `map --focus`) to locate →
`explain` to understand → `calls` / `impact` to see the blast radius → edit →
run the listed tests.

## Examples

```bash
semantiq map --focus src/auth/ --max-tokens 1000
semantiq search "where are file renames handled" --limit 5
semantiq search "retry with backoff" --snippets   # code of each hit
semantiq search "retry with backoff" --file-type rs,ts --json | jq -r '.results[].file_path'
semantiq refs should_exclude_path --json | jq '.usages[] | "\(.file_path):\(.line) \(.kind)"'
semantiq calls index_file --direction callers --max-depth 2
semantiq calls parse --file src/config.rs --direction callees   # one of several `parse`
semantiq hierarchy Storage --json | jq -r '.subtypes[].sub_type'
semantiq impact parse_config --max-depth 3
semantiq dead-code --path-prefix src/ --language rust --include-public
semantiq explain RetrievalEngine
semantiq deps src/server.rs --json | jq '.imported_by'
```

Options shared by all query commands: `--json`, `--no-refresh` (skip the
freshness check, fastest), `-d/--database <file>`, `-p/--project <dir>`.
`search` also takes `-l/--limit` (10), `--min-score` (0.3), `--file-type rs,py`,
`--symbol-kind function,struct,…`, `--snippets`; `refs` takes `-l/--limit`
(30); `impact`
takes `--max-depth` (2, max 4), `--file`, `-l/--limit` (200); `calls` takes
`--direction` (both), `--max-depth` (1, max 3), `--file`, `-l/--limit` (100);
`hierarchy` takes `--max-depth` (3, max 5), `-l/--limit` (200 per direction);
`dead-code` takes `--path-prefix`, `--language`, `--include-public`,
`-l/--limit` (100); `map` takes `--max-tokens` (1500), `--focus`,
`--path-prefix`.

## Reading the results

- **map**: files ranked by how much the rest of the code uses them (PageRank
  over references and imports), each with its key symbols' signatures. With
  `--focus`, the map centers on those files / symbols and their neighbours;
  `unmatched_focus` lists entries that matched nothing.
- **search**: one hit per entry, `path:start-end kind name (score)` then its
  most relevant line (the declaration, or the line with most query words),
  cut to 120 characters; read the file at those lines, or add `--snippets`
  for the code. Hits come from three strategies merged together: symbol names
  (score up to 1.0), semantic similarity (up to 0.95) and plain text (up to
  0.75). Scores are relative to the query: compare hits within one result
  list, not across queries. A hit with `symbol_name` points at a definition.
  `(more exist: raise limit)` / `truncated: true` means the list was cut.
  Few or weak hits? Rephrase as what the code *does*, or fall back to grep.
- **refs**: `definitions` (with the symbol kind and declaration line) vs
  `usages` grouped by file, one `line kind  code` entry each; each usage has a
  `kind`: `call`, `type`, `import`, `reference`. Comments, strings and longer
  names never match.
  `kind: "text"` means the name is not a code symbol (e.g. a JSON/YAML key)
  and was found by text search. Matching is by name: homonyms (two `new`
  methods) share references, so check the file before trusting a hit.
- **calls**: `callers` are call sites of the symbol (`caller` = enclosing
  function, absent for top-level code), `callees` the calls it makes; the
  text lists them by file as `line name`, the queried symbol being implied;
  `depth` 2+ follows callers of callers / callees of callees. Names the
  project does not define (library calls) go to `external_callees`. Each edge
  has the same `confidence` scale as impact. Only real calls count: passing a
  function as a value is a `refs` usage, not a call edge.
- **hierarchy**: `supertypes` (what the type extends / implements) and
  `subtypes` (what extends / implements it), transitively; `kind` is
  `extends` or `implements`, `resolved: false` marks a library type. Covers
  Rust `impl Trait for Type` and supertraits, TS/JS, Python, Java, Kotlin, C#,
  C++, PHP, Ruby, Scala; not Go's implicit interfaces.
- **impact**: sites grouped by file, closest first; sites with the same
  description share one line (`120,134 call index_file in initial_index`).
  `depth` 1 uses the symbol
  directly, 2 uses a direct user, etc. (`enclosing` is the function that
  carries the impact one level further). `confidence`, strongest first:
  `same_file` > `imports` (the file imports the definition) > `unique_name`
  (only one definition has this name) > `name_only` (possibly a homonym: verify
  it). `test_files` lists the tests worth running; `truncated: true` means the
  site limit was hit. Several definitions listed → rerun with `--file`.
- **dead-code**: symbols whose name is referenced nowhere outside their own
  definition, most certain first, each with a `confidence` (`high`, `medium`,
  `low`) and the `reasons` that lower it. Entry points, tests, trait / interface members and
  (without `--include-public`) public symbols are excluded and counted in
  `excluded`. Matching is by name and misses reflection, macros and
  framework registration: confirm with `refs` or grep before deleting.
- **explain**: `found: false` means no symbol with that exact name: try
  `search`. Import statements are listed apart (`imported_in`);
  `related_symbols` are other symbols from the same files.
- **deps**: `imports` (each with `kind`: `local`, `external` or `std`; the
  text groups them by kind and module, `a::{X, Y}`) and `imported_by` (files
  whose local imports resolve to this file, each once). Paths are
  relative to the project root.

## Errors and exit codes

`0` success (even with no results), `1` error, `2` bad usage. Messages go to
stderr. "No Semantiq index found" → run `semantiq index` at the project root
(first run can take a minute; later runs only reindex changed files).

`search` loads the embedding model on each call (~1 s); the other commands
skip it and answer in tens of milliseconds, so prefer `refs`/`calls`/`explain`
when you already know the name. Full JSON field reference:
[REFERENCE.md](REFERENCE.md).
