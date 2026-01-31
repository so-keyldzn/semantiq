# Changelog

All notable changes to Semantiq will be documented in this file.

## [Unreleased]

## [0.5.0] - 2026-01-31

### Added
- **Adaptive ML Thresholds** - Automatic calibration of semantic search thresholds per programming language
  - Bootstrap mode: Collects 100% of distance observations until 500 samples
  - Production mode: Switches to 10% sampling after bootstrap
  - Auto-calibration: Triggers automatically when bootstrap completes
  - Percentile-based thresholds: Uses p90 for max_distance, p10 for min_similarity
  - Per-language calibration with fallback cascade (language → global → defaults)
- **New `calibrate` CLI command** - Manual threshold calibration with `--dry-run` option
- **ML stats in `stats` command** - Shows bootstrap progress, observations per language, calibrated thresholds
- **New database tables** - `distance_observations` and `threshold_calibration` for ML data
- **CI workflows for `dev` branch** - Tests, Clippy, format checks, and multi-platform builds

### Changed
- **Refactored `store.rs`** (2108 lines → 8 modules) - Better code organization
  - `store/mod.rs` - Core IndexStore struct and helpers
  - `store/files.rs` - File operations and parser version management
  - `store/symbols.rs` - Symbol search and insertion
  - `store/chunks.rs` - Chunk operations and embeddings
  - `store/dependencies.rs` - Dependency graph operations
  - `store/observations.rs` - ML distance observation storage
  - `store/calibrations.rs` - Threshold calibration persistence
  - `store/tests.rs` - All unit tests
- **Refactored `engine.rs`** (1049 lines → 5 modules) - Cleaner architecture
  - `engine/mod.rs` - RetrievalEngine struct and construction
  - `engine/search.rs` - Semantic, symbol, and text search
  - `engine/threshold.rs` - Adaptive threshold management
  - `engine/analysis.rs` - References, dependencies, symbol explanation
  - `engine/tests.rs` - Unit tests
- Schema version bumped to 3 (triggers automatic reindex)

## [0.4.0] - 2026-01-28

### Added
- **JSON logging support** - Structured logging throughout the codebase
- **JSON logging by default** for `serve` command - Better integration with log aggregators
- **MCP tests** - Comprehensive test coverage for MCP server functionality
- **CI and security workflows** - Automated testing and security scanning

### Changed
- **`init-cursor` command is now language-agnostic** - Works with any project type
- Updated `deny.toml` to v2 schema

### Fixed
- Cross-platform FFI compatibility using `c_char`
- Clippy compatibility with `is_multiple_of()`
- Cargo audit integration (replaced rustsec/audit-check action)
- Various clippy warnings resolved throughout codebase
- Added CDLA-Permissive-2.0 license for webpki-roots dependency
- Cross-compilation for aarch64-linux using `cross`

## [0.3.4] - 2026-01-20

### Added
- **macOS Intel (x86_64-apple-darwin) support restored** - Binary now available for Intel Macs
- **CI build workflow** - New `build.yml` for testing builds on push/PR without publishing

### Changed
- **ONNX feature now optional** - `--features onnx` required on supported platforms (Apple Silicon, Linux, Windows)
- Intel Mac builds use `StubEmbeddingModel` (no ONNX) due to missing prebuilt binaries
- Updated CI to use `macos-15` runner for Intel Mac cross-compilation

## [0.3.3] - 2026-01-19

### Added
- **Search filtering options** for `semantiq_search` - more precise and relevant results
  - `min_score` - Minimum relevance score threshold (0.0-1.0, default: 0.35)
  - `file_type` - Filter by file extensions (e.g., "rs,ts,py")
  - `symbol_kind` - Filter by symbol type (e.g., "function,class,struct")
- **CLI flags** for search command: `--min-score`, `--file-type`, `--symbol-kind`
- **Smart default exclusions** - Automatically excludes non-code files (.json, .lock, .yaml, .md, .toml, etc.)
- **`SearchOptions` struct** in `semantiq-retrieval` with builder pattern

### Changed
- `RetrievalEngine::search()` now accepts optional `SearchOptions` parameter
- Improved search relevance by filtering low-score results by default
- Removed obsolete `is_code_file()` function in favor of `SearchOptions::accepts_extension()`

### Added (Tests)
- 12 new unit tests for `SearchOptions` in `query.rs`

## [0.3.2] - 2026-01-19

### Added
- **`.gitignore` support in `init-cursor`** - automatically adds Semantiq database entries
  - Creates `.gitignore` if not present
  - Updates existing `.gitignore` preserving original content
  - Skips if entries already present (no duplication)

### Added (Tests)
- 3 new tests for `.gitignore` handling in `init_cursor.rs`

## [0.3.1] - 2026-01-19

### Added
- **New `init-cursor` command** for Cursor/VS Code configuration setup
  - Creates `.cursor/rules/project.mdc` (general project guidelines)
  - Creates `.cursor/rules/semantiq.mdc` (Semantiq MCP tools usage)
  - Creates `.cursor/mcp.json` (MCP server configuration)
  - Creates `.cursorignore` (indexing exclusions)
  - Creates `.vscode/` config (settings, tasks, launch, extensions)
  - Preserves existing files (skip instead of overwrite)

### Changed
- Centralized `DEFAULT_DB_NAME` and path resolution utilities in `common.rs`
- Refactored all CLI commands to use shared utilities
- CLI description now generic ("for a project" instead of "for a Rust project")

### Added (Tests)
- 7 new unit tests for `common.rs` and `init_cursor.rs`

## [0.3.0] - 2026-01-19

### Added
- **sqlite-vec integration** for vector similarity search (384-dim MiniLM-L6-v2 embeddings)
- **Automatic initial indexing** when MCP server starts (no more manual `semantiq index` required)
- **6 new languages**: HTML, JSON, YAML, TOML, Bash, Elixir (total: 15 languages)
- **ripgrep integration** for fast regex text search via `TextSearcher`
- New `search_similar_chunks()` method for semantic vector search
- New `InitialIndexResult` struct for tracking initial indexing progress

### Fixed
- **"Imported by" always empty** in `semantiq_deps` - rewrote `get_dependents()` to match JS/TS import paths (`@/...`, `./...`, `../...`)
- Import path resolution now handles basename matching with multiple extensions

### Changed
- Schema version bumped to 2 (triggers automatic reindex)
- Added `chunks_vec` virtual table for sqlite-vec embeddings
- `start_auto_indexer()` now runs `initial_index()` before watching for changes
- Improved dependency matching with multiple LIKE patterns and post-filtering

## [0.2.9] - 2026-01-19

### Fixed
- Arrow functions (`const fn = () => {}`) now correctly indexed as `function` instead of `variable`
- Function expressions (`const fn = function() {}`) now correctly indexed as `function`

### Changed
- Added `is_function_variable()` helper to detect functions assigned to variables
- Added `arrow_function` and `lexical_declaration` to chunk boundaries for TypeScript/JavaScript
- Bumped `PARSER_VERSION` to 3 (triggers automatic reindex)

## [0.2.8] - 2026-01-18

### Security
- **CRITICAL**: Added SHA-256 checksum verification for ONNX model downloads (TOFU + hardcoded support)
- **CRITICAL**: Added path traversal protection with canonicalization in `validate_path()`
- **HIGH**: Added `MAX_AST_DEPTH=500` recursion limit in parser to prevent stack overflow attacks
- **HIGH**: Added `safe_slice()` function to prevent panic on invalid byte indices
- **HIGH**: Changed model directory fallback from "." to system temp dir (prevents writes to unexpected locations)
- **HIGH**: Added pagination for `get_chunks_with_embeddings()` to prevent memory exhaustion DoS
- **HIGH**: Reduced download size limit from 500MB to 100MB
- **HIGH**: Added restrictive file permissions (0600 on Unix) for downloaded models and database
- **MEDIUM**: Added explicit symlink handling (`follow_links(false)`) to prevent escape from project root

### Changed
- Refactored `download_file()` with connection timeouts (30s connect, 5min global)
- Improved checksum verification with detailed warning messages

## [0.2.7] - 2026-01-18

### Added
- Automatic version update notification at server startup
- Non-blocking background check using GitHub Releases API
- Local cache (24h) to avoid repeated API calls
- `--no-update-check` CLI flag to disable update notifications
- `SEMANTIQ_UPDATE_CHECK` environment variable for configuration

### Changed
- Updated author info to keyldzn

## [0.2.6] - 2026-01-18

### Added
- Automatic reindexation when parser version changes (no more manual `--force` needed)
- `PARSER_VERSION` constant to track parser logic changes
- Support for `const`/`let` variable extraction in TypeScript/JavaScript
- GitHub Sponsors funding configuration

### Changed
- Version detection uses atomic transactions to prevent race conditions
- Documentation updated with known limitations and setup guides

### Fixed
- Filter out verbose ONNX Runtime logs during indexing

## [0.2.4] - 2026-01-18

### Fixed
- Model download failing in async Tokio context (replaced `reqwest::blocking` with `ureq`)
- Download size limit too small for 90MB ONNX model (increased to 200MB)
- ONNX inference crash due to missing `token_type_ids` input
- Embeddings not generated during `semantiq index` command

### Changed
- `semantiq index` now generates embeddings for all chunks
- Centralized file exclusion logic into `exclusions.rs` module
- Auto-indexer and FileWatcher now use shared exclusion patterns

## [0.2.3] - 2026-01-18

### Added
- ONNX embedding model integration for semantic search
- Automatic model download on first run
- Cosine similarity search for vector matching
- Alternative installation via `cargo install --git`
- CHANGELOG.md for version history

### Changed
- Embeddings now generated automatically during auto-indexing
- Switch from OpenSSL to rustls for better cross-compilation support
- Use ort download-binaries for automatic ONNX Runtime provisioning

### Removed
- macOS Intel (x86_64-apple-darwin) binary - ONNX Runtime does not support this target

## [0.2.2] - 2026-01-17

### Changed
- Improved CLAUDE.md template to prioritize Semantiq tools over grep/Glob

## [0.2.1] - 2026-01-17

### Fixed
- Error handling with proper mutex propagation
- SQL injection vulnerability via LIKE escaping
- UTF-8 safety in tree-sitter text extraction
- N+1 query pattern in get_stats() (4 queries → 1)

### Changed
- Shared single `Arc<IndexStore>` instead of 3 separate DB connections
- Improved scoring algorithm with symbol type boosting
- Results limited to 500 to prevent memory issues
- Added `PRAGMA busy_timeout=5000` for concurrent access

## [0.2.0] - 2026-01-17

### Added
- Automatic npm package version update from git tag

## [0.1.3] - 2026-01-17

### Added
- New `semantiq init` command for easy project setup
- Auto-creates `.claude/settings.json`, `CLAUDE.md`, updates `.gitignore`
- Runs initial indexation automatically

## [0.1.2] - 2026-01-17

### Added
- Auto-indexing for real-time file updates
- FileWatcher integration with create/modify/delete events
- Background task with 2-second polling

## [0.1.1] - 2026-01-17

### Added
- npm README documentation
- Updated main README with correct npm package name

## [0.1.0] - 2026-01-17

### Added
- Initial release
- MCP server with 4 tools: search, find_refs, deps, explain
- Support for 9 languages via tree-sitter
- SQLite storage with FTS5 search
