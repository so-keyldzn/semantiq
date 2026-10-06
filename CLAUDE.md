# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
cargo build                          # Build (debug)
cargo build --release                # Build (release, with LTO)
cargo build --no-default-features    # Stub embeddings, no ONNX Runtime (macOS Intel)
cargo test                           # Run all tests (stub embeddings, never downloads the model)
cargo test -p semantiq-parser        # Tests for one crate
cargo test -p semantiq-parser test_language_from_extension  # Single test
cargo check                          # Type-check without building
cargo fmt                            # Format
cargo clippy                         # Lint
```

## CLI Usage

```bash
cargo run -- init                            # First-time setup: skill (.claude/skills/semantiq/), .mcp.json, CLAUDE.md block, .gitignore, indexes
cargo run -- init --no-mcp                   # Skill + CLI only (--no-skill: MCP only, --no-index, --force, --global)
cargo run -- init-cursor                     # Same for Cursor (.cursor/) and VS Code (.vscode/), plus an AGENTS.md block
cargo run -- index /path/to/project          # Index a project
cargo run -- index --force                   # Force full reindex
cargo run -- serve --project /path/to/project  # MCP server (stdio)
cargo run -- serve --project . --http-port 3000  # HTTP API mode
cargo run -- search "query" --json           # Query commands mirror the MCP tools: markdown or --json
cargo run -- refs <symbol>                   # = semantiq_find_refs
cargo run -- deps <file>                     # = semantiq_deps
cargo run -- explain <symbol>                # = semantiq_explain
cargo run -- impact <symbol> --max-depth 3   # = semantiq_impact
cargo run -- stats                           # Index statistics
cargo run -- map --max-tokens 1000 --focus src/x.rs  # Ranked repo map (PageRank, token budget)
cargo run -- calibrate                       # Build adaptive search thresholds (needs 500+ observations)
cargo run -- update                          # Self-update the binary to the latest GitHub release
cargo run -- update --check                  # Only report whether an update is available
```

## Architecture

Semantiq is a Rust workspace providing semantic code understanding for AI coding assistants via MCP (Model Context Protocol).

Query commands (`commands/query.rs`) locate `.semantiq.db` in the cwd or a parent (exit 1 + hint if missing/empty), refresh changed files via `AutoIndexer::initial_index` only when a cheap hash walk finds a difference (`--no-refresh` skips it), and call the same `*_output()` builders as the MCP handlers (`semantiq-mcp/src/server/outputs.rs`). Only `search` loads the embedding model; the others use `RetrievalEngine::without_embeddings`. The Agent Skill lives in `skills/semantiq/` and is embedded by `init` via `include_str!` (the Dockerfile copies `skills/`).

### Crate Structure

```
crates/
├── semantiq/           # CLI binary (clap), HTTP API (axum)
├── semantiq-mcp/       # MCP server (rmcp), tool handlers
├── semantiq-parser/    # Tree-sitter parsing, symbol/chunk/import extraction
├── semantiq-index/     # SQLite storage (rusqlite, FTS5, sqlite-vec)
├── semantiq-retrieval/ # Search engine (3 strategies), query expansion, ranking
└── semantiq-embeddings/# ONNX embedding model (feature-gated, stub by default)
```

### Data Flow

1. **Indexing**: `WalkBuilder` (ignore crate) → `should_exclude_entry()` filter → `Language::from_path()` → content hash check (`needs_reindex`) → tree-sitter parse → `SymbolExtractor` / `ChunkExtractor` / `ImportExtractor` / `ReferenceExtractor` / `StructureExtractor` → `IndexStore` (SQLite with FTS5 triggers + sqlite-vec embeddings)

2. **Search**: `RetrievalEngine::search()` runs 3 strategies sequentially: **semantic** (sqlite-vec KNN) → **symbol** (FTS5 MATCH) → **text** (grep, only if results < limit). Results are deduplicated by `"file_path:start_line:end_line"`, scored, and merged.

   **References**: `find_references()` takes definitions from `symbols` and usages from the `refs` table (AST identifier leaves, one row per name/file/line, classified definition/import/call/type/reference by `ReferenceExtractor` in `semantiq-parser/src/references.rs`). Comments, strings and substrings never match. Names absent from `refs` (data-file keys) fall back to text search (`match_type = "text"`). Resolution is by name only: homonyms share references.

   **Repo map**: `semantiq-retrieval/src/repo_map.rs`. `IndexStore::load_repo_graph()` (`store/graph.rs`, read-only bulk load) → file graph: edges from non-definition `refs` to files defining the name (weighted by `sqrt(count)`, reference-kind fit, Aider's name weighting, `COMMON_NAMES` damped, no edge from production code to test files) plus `dependencies.resolved_path` imports → weighted PageRank with a teleport leak (`OUT_LEAK`) and a teleport vector personalized by `focus` → symbol scores = rank handed down through edges + a share of the file rank → binary search on the number of symbols that fit `max_tokens` (`estimate_tokens` = chars/4). Variables, imports, modules and data-language files (JSON/YAML/TOML/HTML) are left out. `RetrievalEngine::repo_map()` caches the unfocused ranking keyed by `IndexStore::graph_fingerprint()`; `build_repo_map()` works on a bare store (CLI, no embedding model). Only 8% of imports carry a `resolved_path` (cross-crate/package imports are `external`), so the ranking leans on `refs`.

   **Impact**: `analyze_impact()` (`engine/impact.rs`) runs a BFS from a symbol's references to their enclosing symbols, up to `max_depth`. Each site gets a confidence (`same_file` > `imports` > `unique_name` > `name_only`); only non-`name_only` sites propagate, and beyond depth 1 functions/methods are followed through `call` sites only (avoids local-variable homonyms). Not exposed on the REST API, only via MCP and `semantiq impact`.

   **Calls**: `call_graph()` (`engine/calls.rs`) walks the `call_edges` table (each AST `call` reference attached at index time to its innermost enclosing function/method by `StructureExtractor`, `semantiq-parser/src/structure.rs`; caller `''` = top-level code). Callers and callees are resolved by name with the same confidence scale as impact; only non-`name_only` edges are followed beyond depth 1 (max 3), visited definitions are never re-walked (recursion-safe). Callees with no definition in the index are listed in `external_callees`.

   **Hierarchy**: `type_hierarchy()` (`engine/hierarchy.rs`) walks the `type_relations` table (`type_name` extends/implements `super_name`, declared over `line..end_line`): Rust `impl Trait for Type` + supertraits, TS/JS, Python, Java, Kotlin, C#, C++, PHP, Ruby, Scala. Go is out of scope (implicit interfaces). C# extends vs implements is a heuristic (first base, `I`-prefixed names).

   **Dead code**: `find_dead_code()` (`engine/dead_code.rs`) starts from `IndexStore::find_unreferenced_symbols()` (function/method/type symbols whose name has no non-definition ref outside their own span) and excludes entry points, tests (path, `mod tests`, `#[test]`/`@Test`-style attributes read from disk), trait/interface members, Rust trait-impl methods, overrides of project supertypes and, unless `include_public`, public symbols. Confidence is lowered for dynamic languages, public symbols, attributes/decorators (framework or macro registration, e.g. `#[tool]`) and possible overrides of library supertypes.

3. **Serving**: MCP on stdio (`rmcp::transport::stdio()`) OR HTTP (`--http-port`), which serves both the REST API and MCP Streamable HTTP at `/mcp`. These are mutually exclusive modes. The MCP server (rmcp 3.x, `#[tool_router]`) exposes 9 read-only tools: `semantiq_search`, `semantiq_repo_map`, `semantiq_find_refs`, `semantiq_deps`, `semantiq_explain`, `semantiq_impact`, `semantiq_calls`, `semantiq_hierarchy`, `semantiq_dead_code` (handlers in `semantiq-mcp/src/server.rs`, params/outputs in `server/types.rs` and `server/structure_types.rs`, output builders shared with the CLI in `server/outputs.rs` and `server/structure.rs`). Each tool returns markdown text plus `structuredContent` matching its `outputSchema`.

### Languages

19 total via tree-sitter (`semantiq-parser/src/language.rs`). Tous ont une `tags.scm` chargée par `QuerySymbolExtractor` (`semantiq-parser/src/query_extractor.rs`) :
- **Code** (symbols + chunks + imports): Rust, TypeScript, JavaScript, Python, Go, Java, C, C++, PHP, Ruby, C#, Kotlin, Scala, Bash, Elixir.
- **Data** (clés/sections indexées comme Variable/Struct, en plus des chunks + embeddings): HTML, JSON, YAML, TOML.

### Key Internal Conventions

- **DB access**: Always use `IndexStore::with_conn(|conn| { ... })` — never lock the mutex directly. Exception: `check_and_prepare_for_reindex()` for multi-step transactions.
- **FTS5 queries**: Always use `IndexStore::escape_fts5_query()` when passing user input to `symbols_fts MATCH`.
- **Parameterized SQL**: Use `params![]` with positional `?1`, `?2` — never string interpolation.
- **File paths in DB**: Always stored as relative paths from project root (via `strip_prefix`).
- **MCP stdout is reserved** for protocol messages. All logs go to stderr (`tracing` with `.with_writer(std::io::stderr)`). JSON log format is automatic in serve mode.
- **Error handling**: `anyhow::Result` internally. MCP tool handlers return `Result<String, String>` — `Err` strings are deliberately opaque to avoid leaking internals.

### Versioning That Triggers Reindex

- **`PARSER_VERSION`** (`semantiq-parser/src/lib.rs`): Bump when symbol/chunk/import extraction logic changes. Triggers full data clear + reindex on next startup.
- **Schema version** (`semantiq-index/src/schema.rs`): For DB schema changes. Incremental steps in `migrate_schema()` (run before `init_schema()`), version stored in `metadata` table.
- **Embedding model** (`embedding_model_id()` / `EMBEDDING_DIMENSION` in `semantiq-embeddings/src/lib.rs`; the id is resolved at runtime, `"stub"` whenever the stub is selected): stored as `embedding_model` / `embedding_dim` in `metadata`. On mismatch, `init_schema()` drops + recreates `chunks_vec`, clears `distance_observations` / `threshold_calibration` and indexed data, and forces a full reindex. Change `CODERANKEMBED_MODEL_ID` whenever the model or its export changes.

### Embedding Model

- **Feature-gated, on by default**: the `semantiq` binary has `default = ["onnx"]` (forwarding `semantiq-embeddings/onnx`); library crates keep `default = []`. `--no-default-features` builds the `StubEmbeddingModel` (zero vectors, semantic search skipped) — used for `x86_64-apple-darwin`, where `ort` has no prebuilt runtime.
- **Backend selection** (`selected_backend()` in `model.rs`): no `onnx` feature → stub; else `SEMANTIQ_EMBEDDINGS=stub|onnx` overrides; else the `test-stub` feature → stub; else ONNX. Every workspace crate enables `test-stub` in its dev-dependencies, so tests never download the model (`--all-features` turns it on too). Real-model tests: `SEMANTIQ_EMBEDDINGS=onnx cargo test -p semantiq-retrieval --features onnx --test intent_queries`.
- **Stub warning**: `semantic_search_unavailable_reason()` drives the startup WARN (`serve`, `index`), the `Embeddings` section of `semantiq stats`, `semantic_search*` fields of `GET /stats`, and a note appended to the MCP instructions.
- Model: `nomic-ai/CodeRankEmbed`, community INT8 ONNX export (768-dim, ~139MB), downloaded on first run to `dirs::data_dir()/semantiq/models/` (`coderankembed-int8.onnx`, `coderankembed-tokenizer.json`). URLs are pinned to a commit and verified against hard-coded SHA-256 digests (`MODEL_SHA256` / `TOKENIZER_SHA256` in `model.rs`); a mismatch triggers a re-download, and a mismatching download is rejected.
- Single dimension constant: `semantiq_embeddings::EMBEDDING_DIMENSION` (re-exported by `semantiq_index::schema`). Never hard-code it.
- Query vs document: use `embed_query()` for search queries (prepends `"Represent this query for searching relevant code: "`), `embed()` / `embed_batch()` for code chunks (no prefix).
- Pooling: CLS (first token) + L2 normalization, configurable per model via `EmbeddingConfig::pooling` (`Pooling::Cls` | `Pooling::Mean`). Truncation (512 tokens) is done by the tokenizer so `[SEP]` is preserved. `token_type_ids` is only sent if the graph declares it.
- `embed_batch` runs forward passes of at most `batch_size` (32) texts. The INT8 export quantizes activations per batch, so a chunk's vector varies slightly with its batch neighbours (cos ≈ 0.97); accepted for ~1.7x faster indexing.
- ONNX session wrapped in `Mutex<Session>` (not `Send`). Thread count: `SEMANTIQ_ONNX_THREADS` env var (default: `min(cpu_count, 8)`).
- Adaptive thresholds: After 500+ search observations, `semantiq calibrate` computes per-language distance thresholds. Fallback cascade: language-specific → global → hardcoded defaults (`max_distance=1.2`, `min_similarity=0.3`).

### Thread Safety

- `IndexStore`: `Arc<Mutex<Connection>>` — serialized single connection.
- `LanguageSupport`: Wrapped in `Mutex` in `AutoIndexer` (tree-sitter parsers are `!Send`).
- `OnnxEmbeddingModel`: `Mutex<Session>`.
- `RetrievalEngine`: `Arc<RwLock<ThresholdConfig>>` for thresholds, `Mutex<Option<FileListCache>>` (30s TTL) for text search file list, `Mutex<Option<(fingerprint, Arc<RankedRepo>)>>` for the unfocused repo map ranking.

### HTTP API (`--http-port`)

Alternative to MCP stdio. Binds to `127.0.0.1` by default (no auth); `--http-host 0.0.0.0` exposes it to the network. Endpoints: `GET /health`, `GET /stats`, `POST /search`, `POST /map`, `POST /find-refs`, `POST /deps`, `POST /explain`, `POST /calls`, `POST /hierarchy`, `POST /dead-code` (the last three return the MCP tools' structured output, limit capped at 100). MCP Streamable HTTP at `/mcp` (Host header restricted to loopback unless `--http-host` is non-loopback). Middleware: 1MB body limit, 50 concurrent requests, CORS (`--cors-origin` for production).

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `SEMANTIQ_ONNX_THREADS` | `min(cpu_count, 8)` | ONNX intra-op parallelism |
| `SEMANTIQ_EMBEDDINGS` | unset | `stub` forces zero-vector embeddings (no download, semantic search off); `onnx` forces the real model in a `test-stub` build |
| `SEMANTIQ_UPDATE_CHECK` | `true` | `"0"` or `"false"` to disable version check |
| `SEMANTIQ_UPDATE_CACHE_HOURS` | `24` | Hours to cache GitHub version check |
| `RUST_LOG` | `info,ort=warn` | Tracing filter (`--verbose` sets `debug`) |

### Testing Patterns

- **In-memory DB**: `IndexStore::open_in_memory()` is the standard test fixture — no temp files needed for DB tests.
- **MCP server tests**: `create_test_server()` in `server.rs` builds a server without background tasks. Uses `TempDir` for tests needing physical files.
- **Async tests**: MCP tool handlers use `#[tokio::test]`.
- **Parser tests**: `LanguageSupport::new()` + `support.parse(Language::X, source)`.

### Key Types

- `Language` / `LanguageSupport` — Multi-language tree-sitter parsing (`semantiq-parser/src/language.rs`)
- `IndexStore` — SQLite wrapper with FTS5 + sqlite-vec (`semantiq-index/src/store.rs`)
- `RetrievalEngine` — Query execution and 3-strategy ranking (`semantiq-retrieval/src/engine/mod.rs`; submodules `search.rs`, `analysis.rs`, `threshold.rs`)
- `SemantiqServer` — MCP server with tool handlers (`semantiq-mcp/src/server.rs`)
- `AutoIndexer` — File watcher + incremental reindexing (`semantiq-index/src/auto_indexer.rs`)
- `QueryExpander` — snake_case/camelCase/PascalCase/kebab-case conversion (`semantiq-retrieval/src/query.rs`)
