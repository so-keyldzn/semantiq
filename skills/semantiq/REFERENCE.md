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
