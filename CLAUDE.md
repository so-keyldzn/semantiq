# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
cargo build                          # Build (debug)
cargo build --release                # Build (release, with LTO)
cargo build --features semantiq-embeddings/onnx  # Build with real ONNX embeddings
cargo test                           # Run all tests
cargo test -p semantiq-parser        # Tests for one crate
cargo test -p semantiq-parser test_language_from_extension  # Single test
cargo check                          # Type-check without building
cargo fmt                            # Format
cargo clippy                         # Lint
```

## CLI Usage

```bash
cargo run -- init                            # First-time setup: writes .mcp.json, CLAUDE.md, .gitignore, indexes
cargo run -- init-cursor                     # Same for Cursor (.cursor/) and VS Code (.vscode/)
cargo run -- index /path/to/project          # Index a project
cargo run -- index --force                   # Force full reindex
cargo run -- serve --project /path/to/project  # MCP server (stdio)
cargo run -- serve --project . --http-port 3000  # HTTP API mode
cargo run -- search "query"                  # CLI search (testing)
cargo run -- stats                           # Index statistics
cargo run -- calibrate                       # Build adaptive search thresholds (needs 500+ observations)
```

## Architecture

Semantiq is a Rust workspace providing semantic code understanding for AI coding assistants via MCP (Model Context Protocol).

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

1. **Indexing**: `WalkBuilder` (ignore crate) → `should_exclude_entry()` filter → `Language::from_path()` → content hash check (`needs_reindex`) → tree-sitter parse → `SymbolExtractor` / `ChunkExtractor` / `ImportExtractor` → `IndexStore` (SQLite with FTS5 triggers + sqlite-vec embeddings)

2. **Search**: `RetrievalEngine::search()` runs 3 strategies sequentially: **semantic** (sqlite-vec KNN) → **symbol** (FTS5 MATCH) → **text** (grep, only if results < limit). Results are deduplicated by `"file_path:start_line:end_line"`, scored, and merged.

3. **Serving**: MCP on stdio (`rmcp::transport::stdio()`) OR HTTP API (`--http-port`). These are mutually exclusive modes. The MCP server exposes 4 tools: `semantiq_search`, `semantiq_find_refs`, `semantiq_deps`, `semantiq_explain` (all defined in `semantiq-mcp/src/server.rs`).

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
- **Schema version** (`semantiq-index/src/schema.rs`): For DB schema changes. No automatic migration — version stored in `metadata` table.

### Embedding Model

- **Feature-gated**: The `onnx` feature on `semantiq-embeddings` is **off by default**. Without it, `StubEmbeddingModel` returns zero vectors — semantic search runs but produces meaningless results.
- Model: `all-MiniLM-L6-v2` (384-dim, ~90MB), downloaded from HuggingFace on first run to `dirs::data_dir()/semantiq/models/`.
- ONNX session wrapped in `Mutex<Session>` (not `Send`). Thread count: `SEMANTIQ_ONNX_THREADS` env var (default: `min(cpu_count, 8)`).
- Adaptive thresholds: After 500+ search observations, `semantiq calibrate` computes per-language distance thresholds. Fallback cascade: language-specific → global → hardcoded defaults (`max_distance=1.2`, `min_similarity=0.3`).

### Thread Safety

- `IndexStore`: `Arc<Mutex<Connection>>` — serialized single connection.
- `LanguageSupport`: Wrapped in `Mutex` in `AutoIndexer` (tree-sitter parsers are `!Send`).
- `OnnxEmbeddingModel`: `Mutex<Session>`.
- `RetrievalEngine`: `Arc<RwLock<ThresholdConfig>>` for thresholds, `Mutex<Option<FileListCache>>` (30s TTL) for text search file list.

### HTTP API (`--http-port`)

Alternative to MCP stdio. Endpoints: `GET /health`, `GET /stats`, `POST /search`, `POST /find-refs`, `POST /deps`, `POST /explain`. Middleware: 1MB body limit, 50 concurrent requests, CORS (`--cors-origin` for production).

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `SEMANTIQ_ONNX_THREADS` | `min(cpu_count, 8)` | ONNX intra-op parallelism |
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
