# Semantiq `--json` reference

Each query command prints one JSON object on stdout, identical to the
`structuredContent` of the matching MCP tool. Optional fields are omitted when
empty. Line numbers are 1-based; paths are relative to the project root.

## `semantiq search <query> --json`

```jsonc
{
  "query": "where are file renames handled",
  "total_count": 10,
  "search_time_ms": 14,
  "results": [
    {
      "file_path": "crates/semantiq-index/src/watcher.rs",
      "start_line": 40, "end_line": 72,
      "score": 0.95,
      "symbol_name": "poll_events",      // optional: the hit is a definition
      "symbol_kind": "function",         // optional
      "content": "pub fn poll_events(&self) -> Vec<FileEvent> { ..."
    }
  ]
}
```

`score`: symbol matches up to 1.0, semantic up to 0.95, text up to 0.75,
normalized within each strategy (relative to this query only).

## `semantiq refs <symbol> --json`

```jsonc
{
  "symbol": "should_exclude_path",
  "total_count": 19,
  "search_time_ms": 2,
  "definitions": [
    { "file_path": "crates/semantiq-index/src/exclusions.rs", "line": 40,
      "kind": "definition", "content": "pub fn should_exclude_path(path: &Path) -> bool {\n ..." }
  ],
  "usages": [
    { "file_path": "crates/semantiq-index/src/auto_indexer.rs", "line": 2, "kind": "import",
      "content": "use crate::exclusions::{is_file_too_large, should_exclude_entry, should_exclude_path};" }
  ]
}
```

`kind`: `definition`, `call`, `type`, `import`, `reference`, or `text` (name
unknown to the syntax index, found by text search). A definition's `content` is
its full source; a usage's `content` is its line.

## `semantiq deps <file> --json`

```jsonc
{
  "file_path": "crates/semantiq-index/src/watcher.rs",
  "imports": [
    { "target_path": "crate::exclusions::should_exclude_path", "import_name": "should_exclude_path", "kind": "local" },
    { "target_path": "anyhow::Result", "import_name": "Result", "kind": "external" }
  ],
  "imported_by": [ "crates/semantiq-index/src/auto_indexer.rs", "crates/semantiq-index/src/lib.rs" ]
}
```

`target_path` is the import as written (module path, relative file…), not
always a project file. `imported_by` has one entry per importing statement, so
a file can appear more than once. Both are `null` if the lookup failed (not
merely empty); a file absent from the index gives two empty lists.

## `semantiq explain <symbol> --json`

```jsonc
{
  "symbol": "RetrievalEngine",
  "found": true,
  "definitions": [
    { "file_path": "crates/semantiq-retrieval/src/engine/mod.rs", "kind": "struct",
      "start_line": 41, "end_line": 51,
      "signature": "pub struct RetrievalEngine {",                // optional
      "doc_comment": "/// The main search and retrieval engine." }  // optional
  ],
  "usage_count": 25,
  "related_symbols": [ "FileListCache", "FILE_LIST_CACHE_TTL_SECS" ]
}
```

`definitions` may include `import` entries (files that import the name).
`related_symbols`: up to 10 other symbols from the files of those definitions,
sorted.

## `semantiq impact <symbol> --json`

```jsonc
{
  "symbol": "should_exclude_path",
  "definitions": [ { "file_path": "crates/semantiq-index/src/exclusions.rs", "line": 40, "kind": "function" } ],
  "site_count": 20,
  "files": [
    {
      "file_path": "crates/semantiq-index/src/auto_indexer.rs",
      "is_test": false,
      "depth": 1,
      "sites": [
        { "line": 231, "depth": 1, "target": "should_exclude_path", "kind": "call",
          "enclosing": "index_file", "confidence": "imports" }
      ]
    }
  ],
  "test_files": [ "crates/semantiq-index/src/exclusions.rs", "crates/semantiq-index/src/watcher.rs" ],
  "truncated": false
}
```

- `depth`: 1 = uses the symbol directly; n = uses an impacted symbol of depth n-1.
- `enclosing`: the function/type containing the site, impacted at the next depth.
- `is_test`: heuristic from the path (`tests/`, `test_*`, `*_test.*`,
  `*.spec.*`, `tests.rs`…) or a site inside a `test_*`/`Test*` function;
  such files are listed in `test_files`.
- `confidence`: `same_file` > `imports` > `unique_name` > `name_only`. Only
  sites above `name_only` propagate to the next depth; beyond depth 1, only
  `call` sites of functions/methods are followed.
- `truncated`: the site limit (`--limit`, default 200, max 1000) was reached.

## `semantiq map --json`

```jsonc
{
  "max_tokens": 256,
  "estimated_tokens": 251,            // characters / 4
  "total_files": 107,                 // after --path-prefix
  "total_symbols": 1513,
  "shown_symbols": 9,
  "focus_files": [ "crates/semantiq-index/src/auto_indexer.rs" ],
  "focus_symbols": [],
  "unmatched_focus": [ "NoSuchThing" ],
  "files": [
    {
      "file_path": "crates/semantiq-index/src/auto_indexer.rs",
      "language": "rust",             // optional
      "rank": 0.175043,
      "symbols": [
        { "name": "remove_file", "kind": "method", "line": 432,
          "parent": "AutoIndexer",                               // optional: enclosing type
          "signature": "fn remove_file(&self, path: &Path) -> Result<()>",
          "doc": "Remove a file from the index",                 // optional, first line
          "rank": 0.044485 }
      ]
    }
  ]
}
```

- `files` are listed in directory order; `rank` is the file's PageRank in the
  graph of references and imports (higher = more used by the rest of the
  code), personalized toward `--focus` entries when given.
- A symbol's `rank` is the share of importance it receives through
  references. The map keeps the best symbols that fit `--max-tokens`
  (default 1500, 256 to 8000).
- `focus_files` / `focus_symbols`: how `--focus` entries were resolved
  (directories expand to their files); `unmatched_focus` matched nothing.
- Variables, imports, modules and data files (JSON, YAML, TOML, HTML) are
  left out. The markdown output (without `--json`) is the rendered map.

## `semantiq calls <symbol> --json`

```jsonc
{
  "symbol": "index_file",
  "direction": "both",                 // callers, callees or both
  "definitions": [ { "file_path": "crates/semantiq-index/src/auto_indexer.rs", "line": 218, "kind": "method" } ],
  "callers": [
    { "depth": 1, "caller": "process_events", "callee": "index_file",
      "file_path": "crates/semantiq-index/src/auto_indexer.rs", "line": 186, "confidence": "same_file" }
  ],
  "callees": [
    { "depth": 1, "caller": "index_file", "callee": "to_relative_string",
      "file_path": "crates/semantiq-index/src/auto_indexer.rs", "line": 220, "confidence": "unique_name" }
  ],
  "external_callees": [ "read_to_string", "lock" ],
  "truncated": false
}
```

- Each edge is one call site: `caller` (the enclosing function or method;
  omitted for top-level code) calls `callee` at `file_path:line`.
- `depth`: 1 = calls the symbol / made by the symbol; n = one hop further
  (callers of callers, callees of callees), up to `--max-depth` (max 3).
- `confidence`: how the edge was resolved to the definition, `same_file` >
  `imports` > `unique_name` > `name_only` (possibly a homonym). Only edges
  above `name_only` are followed beyond depth 1.
- `external_callees`: names the symbol calls that the index does not define
  (standard library, dependencies), sorted.
- `truncated`: the edge limit (`--limit`, default 100, max 1000) was reached.

## `semantiq hierarchy <type> --json`

```jsonc
{
  "symbol": "EmbeddingModel",
  "definitions": [ { "file_path": "crates/semantiq-embeddings/src/model.rs", "line": 281, "kind": "trait" } ],
  "supertypes": [
    { "depth": 1, "sub_type": "EmbeddingModel", "super_type": "Send", "kind": "extends",
      "file_path": "crates/semantiq-embeddings/src/model.rs", "line": 281, "resolved": false }
  ],
  "subtypes": [
    { "depth": 1, "sub_type": "StubEmbeddingModel", "super_type": "EmbeddingModel", "kind": "implements",
      "file_path": "crates/semantiq-embeddings/src/model.rs", "line": 317, "resolved": true }
  ],
  "truncated": false
}
```

- Each relation reads `sub_type` `kind` (`extends` / `implements`)
  `super_type`, declared at `file_path:line` (class header, `impl` block).
- `depth`: 1 = direct relation, n = through n-1 intermediate types, up to
  `--max-depth` (default 3, max 5).
- `resolved`: the type at the far end is defined in the project; `false` is
  a library type (`Send`, `Exception`…), not walked further.
- Rust: `impl Trait for Type` gives `implements`, supertraits give `extends`.
  C#: extends vs implements is a heuristic (first base, `I`-prefixed names).
- `truncated`: the limit per direction (`--limit`, default 200) was reached.

## `semantiq dead-code --json`

```jsonc
{
  "symbols": [
    { "name": "unwatch", "kind": "method",
      "file_path": "crates/semantiq-index/src/watcher.rs", "start_line": 46, "end_line": 50,
      "signature": "pub fn unwatch(&mut self, path: &Path) -> Result<()> {",   // optional
      "confidence": "low",
      "reasons": [ "no reference outside its own definition", "public / exported: other packages may use it" ] }
  ],
  "candidates": 524,
  "excluded": { "entry_points": 2, "tests": 517, "public": 0, "trait_members": 2 },
  "truncated": true
}
```

- `symbols`: functions, methods and types whose name has no reference outside
  their own definition, most certain first.
- `confidence`: `high`, `medium` or `low`; `reasons` says why the symbol is
  reported and what lowers the confidence (dynamic language, public symbol,
  attribute / decorator that may register it, possible override of a library
  type).
- `candidates`: unreferenced definitions examined; `excluded` counts those
  kept as alive by convention: `entry_points` (`main`, constructors, dunder
  methods), `tests`, `public` (unless `--include-public`), `trait_members`
  (trait / interface declarations, trait impls, overrides).
- `truncated`: the limit (`--limit`, default 100, max 1000) was reached.
