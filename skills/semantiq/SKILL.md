---
name: semantiq
description: Semantic code intelligence for the current project through the `semantiq` CLI (run via Bash). Use it when you do not know the exact name or string to grep for ("where are file renames handled?", "how is auth wired?"), to find every real usage of a symbol before renaming or changing it, to answer "who uses X / what breaks if I change X" with the tests to run, to see what a file imports and which files import it, or to get a symbol's signature and docs without opening files. Do NOT use it for exact-string or regex lookups (an error message, a config key, a TODO): plain grep/rg is faster and exhaustive there. Requires a `.semantiq.db` index in the project (created by `semantiq index` or `semantiq init`).
---

# Semantiq CLI

`semantiq` answers questions about the code from a local index (`.semantiq.db`,
found in the current directory or a parent). Each command refreshes files
changed since the last run, then prints results on stdout. Add `--json` to get
structured output (same schema as the MCP tools), ideal for `jq` and chaining.

## Pick the command

| You need… | Run |
|---|---|
| Code for a concept, without knowing names | `semantiq search "<what the code does>"` |
| A symbol whose name you only half know | `semantiq search "<name fragment>" --symbol-kind function` |
| Every definition and usage of a symbol | `semantiq refs <symbol>` |
| What breaks if a symbol changes + tests to run | `semantiq impact <symbol>` |
| Signature, docs, usage count of a symbol | `semantiq explain <symbol>` |
| What a file imports / which files import it | `semantiq deps <path/from/project/root>` |
| An exact string, regex, error text, config key | `grep -rn` / `rg` (not semantiq) |

Typical flow before a change: `search` to locate → `explain` to understand →
`impact` (or `refs`) to see the blast radius → edit → run the listed tests.

## Examples

```bash
semantiq search "where are file renames handled" --limit 5
semantiq search "retry with backoff" --file-type rs,ts --json | jq -r '.results[].file_path'
semantiq refs should_exclude_path --json | jq '.usages[] | "\(.file_path):\(.line) \(.kind)"'
semantiq impact parse_config --max-depth 3
semantiq impact open --file src/store.rs     # one of several `open` definitions
semantiq explain RetrievalEngine
semantiq deps src/server.rs --json | jq '.imported_by'
```

Options shared by all query commands: `--json`, `--no-refresh` (skip the
freshness check, fastest), `-d/--database <file>`, `-p/--project <dir>`.
`search` also takes `-l/--limit` (10), `--min-score` (0.3), `--file-type rs,py`,
`--symbol-kind function,struct,…`; `refs` takes `-l/--limit` (50); `impact`
takes `--max-depth` (2, max 4), `--file`, `-l/--limit` (200).

## Reading the results

- **search**: hits come from three strategies merged together: symbol names
  (score up to 1.0), semantic similarity (up to 0.95) and plain text (up to
  0.75). Scores are relative to the query: compare hits within one result
  list, not across queries. A hit with `symbol_name` points at a definition.
  Few or weak hits? Rephrase as what the code *does*, or fall back to grep.
- **refs**: `definitions` vs `usages`; each usage has a `kind`: `call`, `type`,
  `import`, `reference`. Comments, strings and longer names never match.
  `kind: "text"` means the name is not a code symbol (e.g. a JSON/YAML key)
  and was found by text search. Matching is by name: homonyms (two `new`
  methods) share references, so check the file before trusting a hit.
- **impact**: sites grouped by file, closest first. `depth` 1 uses the symbol
  directly, 2 uses a direct user, etc. (`enclosing` is the function that
  carries the impact one level further). `confidence`, strongest first:
  `same_file` > `imports` (the file imports the definition) > `unique_name`
  (only one definition has this name) > `name_only` (possibly a homonym: verify
  it). `test_files` lists the tests worth running; `truncated: true` means the
  site limit was hit. Several definitions listed → rerun with `--file`.
- **explain**: `found: false` means no symbol with that exact name: try
  `search`. `definitions` can include `import` entries; `related_symbols` are
  other symbols from the same files.
- **deps**: `imports` (each with `kind`: `local`, `external` or `std`) and
  `imported_by` (files whose local imports resolve to this file). Paths are
  relative to the project root.

## Errors and exit codes

`0` success (even with no results), `1` error, `2` bad usage. Messages go to
stderr. "No Semantiq index found" → run `semantiq index` at the project root
(first run can take a minute; later runs only reindex changed files).

`search` loads the embedding model on each call (~1 s); the other commands
skip it and answer in tens of milliseconds, so prefer `refs`/`explain` when you
already know the name. Full JSON field reference:
[REFERENCE.md](REFERENCE.md).

<!--
TODO(coordinator): upcoming commands, added by parallel workers. Fill in a row
in "Pick the command", an example and a "Reading the results" entry for each
once merged:
- `semantiq calls <symbol>`     : call graph (callers / callees)
- `semantiq hierarchy <type>`   : type hierarchy (implements / extends)
- `semantiq dead-code`          : symbols with no usage
- `semantiq map`                : repository map (files → main symbols)
-->
