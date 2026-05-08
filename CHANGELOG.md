# Changelog

All notable changes to Semantiq will be documented in this file.

## [Unreleased]

## [0.8.0] - 2026-05-08

Bugfix release that repairs the semantic search pipeline. Six out of seven
natural-language ("intent") queries returned zero results on real codebases
because of accumulated orphan rows in the sqlite-vec virtual table. After
upgrading, the v4→v5 migration runs automatically on first start and cleans
the existing residue.

### Fixed
- **`chunks_vec` orphan rows**. sqlite-vec virtual tables don't honor
  `FK ON DELETE CASCADE`, so every prior `INSERT INTO chunks` (after the
  delete-then-reinsert pattern in `insert_chunks`) left the previous
  chunk's vector behind. Over many reindex cycles those orphans
  dominated the KNN top-k and silently broke semantic search. All
  delete paths (`insert_chunks`, `delete_file`, `clear_all_data`,
  `check_and_prepare_for_reindex`) now purge `chunks_vec` in the same
  transaction. A new integration test (`vec_invariant`) pins the
  contract.
- **Ghost files with absolute paths**. The legacy
  `path.strip_prefix(root).unwrap_or(path)` pattern silently fell through
  to absolute paths when `strip_prefix` failed, leaving duplicate rows
  in `files`. Replaced everywhere with `paths::to_relative_string`,
  which warns instead of failing silently.
- **`pub use` re-exports not extracted**. `parse_rust_use_path` stripped
  `"use "` first, so any line starting with `pub` (or `pub(crate)`,
  `pub(super)`, `pub(in path)`) returned `None`. This broke
  `semantiq_deps` on every Rust `lib.rs`, since those files are mostly
  re-exports.
- **`symbol_kind` missing in semantic search results**. `search_semantic`
  hard-coded `symbol_kind: None`, so output showed `Symbol: foo (unknown)`
  even though the kind was already in the database. Now batch-fetched
  per file and propagated.

### Added
- **Schema v4 → v5 migration**: purges existing `chunks_vec` orphans and
  ghost rows with absolute paths (POSIX, Windows drive paths, UNC).
  Wrapped in a single transaction for crash safety. Visible at startup
  as e.g. `Migrating schema v4 -> v5 (a): purged N orphan rows`.
- `IndexStore::count_orphan_chunk_vectors()` for diagnostics. The
  `RetrievalEngine` calls it at startup and emits a `WARN` if the
  invariant is broken.
- New `paths::to_relative_string` helper in `semantiq-index`. Tries a
  literal `strip_prefix` first and only canonicalizes on the fallback
  path, keeping the hot indexing loop free of `realpath` syscalls.

### Performance
- ~6× lower latency on natural-language queries via the MCP server now
  that the KNN top-k surfaces real chunks instead of zombie zero-vectors
  (28–93 ms vs 78–123 ms previously, on the reference repo).

### Migration notes
- The v4→v5 migration runs automatically the first time a v0.8.0 binary
  opens an existing database. It is idempotent and crash-safe (single
  `BEGIN IMMEDIATE` transaction). Expect a one-shot log line reporting
  how many rows were swept; on heavily-reindexed databases this can be
  in the thousands.
- No changes to MCP tool signatures or output schema. Existing clients
  keep working; output for `semantiq_search` now includes meaningful
  `symbol_kind` values where it previously said `unknown`.

## [0.7.0] - 2026-05-07

### Changed — BREAKING (extraction de symboles)
- **`PARSER_VERSION` 5 → 6** — déclenche un reindex complet automatique au prochain démarrage.
- Migration de tous les langages (18) vers l'extraction par tree-sitter queries
  (`crates/semantiq-parser/queries/<lang>/tags.scm`). Le parcours AST récursif legacy
  reste accessible en interne (`pub(crate) extract_legacy`) comme oracle pour les tests.

#### Changements de format observables
- **Imports** : `name` extrait = nom court (dernier segment du path) au lieu du
  texte entier. Exemples :
  - Rust : `use std::collections::HashMap;` → `name = "HashMap"` (avant : la déclaration entière)
  - Python : `import os` → `name = "os"`
  - PHP : `use Foo\Bar;` → `name = "Bar"`
- **Kotlin** : `interface Greeter` est maintenant capturé comme `Interface`
  (avant : `Class`). `enum class Status` est maintenant `Enum` (avant : `Class`).
  Les méthodes dans `class_body` sont `Method` (avant : `Function`).
  Les imports Kotlin (`import x.y.z`) sont désormais capturés.
- **C++** : les méthodes inline (`class C { int add(int) {} }`) sont maintenant
  extraites comme `Method` avec le parent classe. Avant : non capturées.
  Destructeurs (`~C`) et opérateurs (`operator+`) sont aussi capturés.
- **Elixir** : `defmodule X` est `Module` (avant : `Function`). `def`, `defp`,
  `defmacro`, `defmacrop` sont tous capturés. Le `parent` des `def` reflète
  le `defmodule` englobant ; les modules imbriqués utilisent `.` comme séparateur
  (`MyApp.Outer.Inner`) au lieu de `::`.
- **Python** : les méthodes décorées (`@staticmethod def m`) ne sont plus
  doublées en `Method` + `Function` ; un seul `Method` est extrait.
- **HTML** : seuls les éléments **top-level** (enfants directs de `document`)
  sont extraits. Évite l'explosion d'index sur du HTML réel (auparavant chaque
  `<div>`/`<p>` imbriqué devenait un symbole).
- **JSON / YAML / TOML** : les clés imbriquées ont désormais un `parent` au
  format dot-separated (`a.b.c`). Avant : `parent = None` pour toutes.
- **Rust** : `impl_item` n'est plus extrait comme `Class` parasite.

#### Hardening
- `QuerySymbolExtractor::new()` **panique** désormais si une query .scm échoue
  à compiler, avec la liste exhaustive des erreurs. Avant : `tracing::warn!`
  silencieux + dégradation cachée vers le legacy.
- Suppression de l'instance dupliquée de `QuerySymbolExtractor` dans
  `LanguageSupport` ; une seule source de vérité via le `OnceLock` global.

## [0.6.2] - 2026-05-04

### Security
- **HIGH**: Bumped `rustls-webpki` 0.103.9 → 0.103.13 to fix four CVEs:
  - RUSTSEC-2026-0049 — CRLs not considered authoritative by Distribution Point due to faulty matching logic
  - RUSTSEC-2026-0098 — Name constraints for URI names were incorrectly accepted
  - RUSTSEC-2026-0099 — Name constraints accepted for certificates asserting a wildcard name
  - RUSTSEC-2026-0104 — Reachable panic in certificate revocation list parsing
- **LOW**: Bumped `rand` 0.9.2 → 0.9.4 (RUSTSEC-2026-0097, unsoundness with custom logger)
- Bumped `openssl` 0.10.75 → 0.10.79

### Changed
- Split `crates/semantiq-mcp/src/server.rs` test module into per-tool files
  (`server/tests/{search,find_refs,deps,explain,server_handler,edge_cases}.rs`).
  `server.rs` shrinks from 931 to 479 lines; no behavior change.

## [0.6.1] - 2026-05-04

### Fixed
- Persist `SCHEMA_VERSION` in `metadata` after migration so future migrations can correctly detect that `v3 → v4` was applied
- `IndexStore::open_in_memory` now runs `migrate_schema`, so test fixtures exercise the migration path
- Added regression tests for path-traversal-escaping imports and Python 3-dot relative imports (`from ...top`)

### Changed
- Extracted `PYTHON_STD_MODULES` from `imports.rs` (1148 → 929 lines) into a dedicated `python_stdlib` module
- Converted 10 `unused_self` methods to associated functions in `ChunkExtractor`, `QueryExpander`, `RetrievalEngine`, and `ThresholdCalibrator`
- Tightened visibility of internal items in `semantiq` and `semantiq-retrieval` from `pub` to `pub(crate)` / `pub(super)`
- Replaced wildcard `use super::types::*` in HTTP routes with explicit imports
- `resolve_python_import` now returns `Vec<PathBuf>` instead of always-`Some` `Option<Vec<PathBuf>>`

## [0.6.0] - 2026-02-18

### Added
- **HTTP API server** — Alternative to MCP stdio with `--http-port`, endpoints: `/health`, `/stats`, `/search`, `/find-refs`, `/deps`, `/explain`. Middleware: 1MB body limit, 50 concurrent requests, CORS configurable (`--cors-origin`)
- **Local import resolution** — Resolution of local import paths to actual files on disk (JS/TS, Python, Rust, Go)
- **`resolved_path` column** — Dependencies now store the resolved path, improving `find_refs` accuracy
- **Schema migration v3→v4** — Automatic incremental migration (adds `resolved_path` column)
- **Python stdlib detection** — Accurate classification of Python standard vs external imports (200+ modules, binary search)
- **Symbol parent tracking** — Symbols now include their parent (e.g., method → struct/class)
- **Dockerfile** — Multi-stage Docker image for deployment (Railway-ready)

### Changed
- Bump schema version 3 → 4 (automatic migration, no reindex required)
- Auto-indexer and CLI `index` command use local import resolution

### Fixed
- Correct git clone URL in Dockerfile
- Resolve clippy `module_inception` warning in HTTP tests
- Bump Rust version in Dockerfile to support edition 2024 and let-chains

## [0.5.2] - 2026-02-10

### Security
- **HIGH**: Fixed ReDoS vulnerability - user input is now escaped with `regex::escape()` in `TextSearcher::search()` before regex compilation
- **HIGH**: Updated `bytes` crate 1.11.0 → 1.11.1 to fix integer overflow in `BytesMut::reserve` (RUSTSEC-2026-0007)
- **HIGH**: Text search walker now uses `hidden(true)` and `should_exclude_entry` filtering, preventing reads from `.env`, `.git/`, and other sensitive directories
- **MEDIUM**: Fixed path traversal in `read_file_lines()` - paths are now canonicalized and verified to stay within the project root
- **MEDIUM**: Added input validation (empty, length ≤ 500, limit ≤ 1000) to `semantiq_find_refs`, `semantiq_explain`, and `semantiq_deps` MCP handlers
- **MEDIUM**: Added path traversal rejection (`..`) in `semantiq_deps` file path parameter
- **MEDIUM**: `resolve_project_root()` now canonicalizes paths to normalize `..` components and symlinks
- **LOW**: FTS5 query escaping now strips null bytes and control characters
- **LOW**: Query expansion limited to 10 terms to prevent amplification attacks
- **LOW**: MCP error messages sanitized to avoid leaking internal file paths
- **LOW**: Version check HTTP response limited to 10KB to prevent memory exhaustion
- **LOW**: Poisoned mutex recovery in `DistanceCollector` now logs warnings instead of silently continuing

### Changed
- Capped `limit` parameter to 1000 on all MCP tool handlers at the server level

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
