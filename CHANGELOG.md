# Changelog

All notable changes to Semantiq will be documented in this file.

## [Unreleased]

### Fixed
- **Large data files no longer slow indexing down**: JSON, YAML and TOML files
  above 256 KB (`MAX_DATA_FILE_SIZE`) are skipped; code files keep the 1 MB
  limit. A single 840 KB JSON file held 52 % of this repository's chunks:
  a full index now takes 56 s instead of 243 s. Oversized files indexed by an
  earlier version (or that grew past the limit) are removed from the index on
  the next run.

## [0.10.0] - 2026-10-06

Pivot release: Semantiq moves from "semantic search over MCP" to structural
code intelligence usable three ways (MCP, CLI + Agent Skill, HTTP), backed by
a code-specific embedding model that is now on by default.

### Upgrade notes
- **One-time full reindex** on first start: schema v5 -> v8 and
  `PARSER_VERSION` 8 -> 11 (new embedding space, AST references, call edges,
  type relations). Nothing to do by hand.
- **New embedding model download** (~139 MB, CodeRankEmbed INT8, SHA-256
  pinned) on first run, cached in the data directory. Set
  `SEMANTIQ_EMBEDDINGS=stub` to skip it (semantic search off).
- **HTTP API listens on `127.0.0.1` by default** (was `0.0.0.0`); pass
  `--http-host 0.0.0.0` to expose it. The API is unauthenticated.
- **MCP**: `rmcp` 0.1 -> 3.5. Tools now return `structuredContent` with an
  `outputSchema`, plus read-only annotations; `semantiq serve --http-port`
  also serves MCP Streamable HTTP at `/mcp`. MCP tools: `semantiq_search`,
  `semantiq_repo_map`, `semantiq_find_refs`, `semantiq_deps`,
  `semantiq_explain`, `semantiq_impact`, `semantiq_calls`,
  `semantiq_hierarchy`, `semantiq_dead_code`.
- `semantiq init` now also installs the `semantiq` Agent Skill and rewrites
  its `CLAUDE.md` section as a marked block (see below); re-run it in existing
  projects to pick up the skill.

### Changed
- **Code-specific embedding model**: all-MiniLM-L6-v2 (384-D) replaced by
  nomic-ai/CodeRankEmbed INT8 (768-D, CLS pooling, query prefix via the new
  `EmbeddingModel::embed_query`). Model and tokenizer URLs are pinned to
  immutable revisions and verified against hard-coded SHA-256 digests. On a
  local benchmark (doc comment -> function), R@1 goes from 0.60 to 0.90.
  Schema v6 rebuilds `chunks_vec` at 768 dimensions and forces a full
  reindex; the active model id is stored in metadata so future model changes
  (including stub -> ONNX builds) trigger the same rebuild.
- HTTP API binds to `127.0.0.1` by default; use `--http-host` to expose it.
- README: indexing before `serve` is optional, `serve` indexes on startup (#17).
- `semantiq init` writes its `CLAUDE.md` instructions as a marked block
  (`<!-- semantiq:start -->`) refreshed in place on re-run and appended to an
  existing `CLAUDE.md` (previously skipped); it now points at the CLI/skill
  as well as the MCP tools. An unmodified CLAUDE.md from an older `init` is
  replaced. Re-running `init` leaves an identical `.mcp.json` untouched.
- `.gitignore` entry is now `.semantiq.db*` (covers the WAL/SHM files).
- **`semantiq` skill**: covers `calls`, `hierarchy`, `dead-code` and `map`
  (command table, examples, how to read the results, JSON fields in
  `REFERENCE.md`). Its description now lists the structural and conceptual
  questions that should trigger it ("who calls", "what implements", unused
  code, "where is X handled", before renaming or changing a symbol): the
  agent benchmark never loaded the previous one.
- The `CLAUDE.md` block written by `semantiq init`, the `AGENTS.md` block and
  the Cursor rules of `init-cursor` now say *when* to prefer Semantiq (real
  references, impact, calls, hierarchy, dead code, concepts with no keyword)
  and to keep grep for exact strings, instead of "use Semantiq first", which
  doubled the cost in the benchmark without better answers. They mention the
  new commands and tools.
- The global `--json` flag prints results as JSON for query commands (it
  still switches logs to JSON for the others); query commands log warnings
  and errors only unless `--verbose`.

- **Real embeddings by default**: the `onnx` feature is now a default feature
  of the `semantiq` binary, so `cargo install` / `cargo build` get
  CodeRankEmbed instead of the zero-vector stub. Opt out with
  `--no-default-features` (used for the `x86_64-apple-darwin` release, which
  `ort` cannot target). CI and release workflows drop the redundant
  `--features onnx`.
- The embedding model id written to the index is resolved at runtime
  (`embedding_model_id()` replaces the `EMBEDDING_MODEL_ID` const), so
  switching between the stub and the real model always rebuilds the vectors.

### Added
- **AST-based references**: `semantiq_find_refs` (and `semantiq refs`) read
  identifier occurrences from the syntax tree (`refs` table) instead of text
  search, so comments, strings and longer names never match; each usage is
  tagged `call`, `type`, `import` or `reference`.
- **Change impact**: `semantiq_impact` / `semantiq impact` lists the places
  that may break when a symbol changes (transitive, up to `max_depth`),
  grouped by file, with a confidence per site and the test files to run.
- **Claude Code plugin**: install the skill and the MCP server from Claude
  Code with `/plugin marketplace add so-keyldzn/semantiq` then
  `/plugin install semantiq@semantiq` (the `semantiq` binary is still
  installed separately).
- **Agent benchmark** (`bench/agent/`): reproducible harness running Claude
  Code headless with and without Semantiq (MCP, guided, CLI + skill), with
  automatic scoring and a first 300-run report.
- **Structural intelligence tools** (MCP, REST and CLI):
  - `semantiq_calls` / `POST /calls` / `semantiq calls`: callers and callees of a function or
    method up to 3 levels, each edge with a resolution confidence
    (`same_file`, `imports`, `unique_name`, `name_only`); library calls are
    summarised separately.
  - `semantiq_hierarchy` / `POST /hierarchy` / `semantiq hierarchy`: supertypes and subtypes /
    implementors of a type, transitively (Rust `impl Trait for Type` and
    supertraits, TS/JS, Python, Java, Kotlin, C#, C++, PHP, Ruby, Scala; Go's
    implicit interfaces are out of scope).
  - `semantiq_dead_code` / `POST /dead-code` / `semantiq dead-code`: functions, methods and types
    with no reference outside their definition, excluding entry points,
    tests, trait members and (by default) public symbols, each with a
    confidence and reasons.
  `PARSER_VERSION` 10 → 11 and schema v8 (`call_edges`, `type_relations`
  tables) trigger a one-time full reindex.
  The CLI commands take the same parameters as the tools, print their
  structured output with `--json` (shared `calls_output` /
  `hierarchy_output` / `dead_code_output` builders) and skip the embedding
  model.
- **CLI for every MCP tool**: `semantiq refs`, `deps`, `explain` and `impact`
  join `search`, all with `--json` printing the exact structured output of the
  matching MCP tool (shared `*_output()` builders in `semantiq-mcp`). They find
  `.semantiq.db` in the current directory or a parent, reindex changed files
  before answering (`--no-refresh` to skip), exit 1 with a `semantiq index`
  hint when the index is missing or empty, and 2 on usage errors. Only
  `search` loads the embedding model (new
  `RetrievalEngine::without_embeddings`): the others answer in ~20 ms.
- **`semantiq` Agent Skill** (`skills/semantiq/SKILL.md` + `REFERENCE.md`):
  when to use each command rather than grep and how to read scores and
  confidences. `semantiq init` installs it in `.claude/skills/semantiq/`
  (kept if edited, unless `--force`); `semantiq init --global` installs it in
  `~/.claude/skills/semantiq/`.
- `semantiq init --no-mcp` (skill only), `--no-skill` (MCP only) and
  `--no-index`. `init-cursor` adds a Semantiq section to `AGENTS.md`.
- `SEMANTIQ_EMBEDDINGS=stub` forces the stub model (no download, semantic
  search off); `SEMANTIQ_EMBEDDINGS=onnx` forces the real one. The test suite
  uses the stub through a dev-only `test-stub` feature and never downloads the
  model.
- When semantic search is unavailable (stub build or override), `serve` and
  `index` log a warning at startup, `semantiq stats` shows an `Embeddings`
  section, `GET /stats` reports `embedding_model`, `semantic_search` and
  `semantic_search_unavailable_reason`, and the MCP instructions say so.
- **Repository map**: new MCP tool `semantiq_repo_map`, CLI `semantiq map` and
  HTTP `POST /map`. Ranks files with PageRank over AST references and resolved
  imports (personalized by an optional `focus` list of files, directories or
  symbols), then renders the signatures and first doc line of the most used
  symbols, grouped by directory, within `max_tokens` (default 1500, 256-8000,
  chars / 4). Deterministic, no schema change; the unfocused ranking is cached
  until the index changes. Server instructions now suggest calling it first on
  an unfamiliar repository.
- MCP tool responses start with a `⏳ Initial indexing in progress` notice
  while the startup index pass runs, and `GET /stats` reports `indexing` (#17).

### Fixed
- File watcher: exclusions are evaluated on the path relative to the project
  root, so projects living under a hidden or excluded directory (e.g.
  `~/.config/app`, a `.tmpXXXX` dir) are auto-indexed again.
- Renamed, moved or deleted files are removed from the index (watcher events
  on vanished paths, and files missing at startup are pruned); unchanged
  files are no longer re-parsed and re-embedded on every watcher event.
- Semantic search keeps the nearest chunks (results were truncated in rowid
  order), and `min_score` is applied to raw per-strategy scores, so the
  weakest hit of each strategy is no longer always dropped.
- MCP and HTTP handlers run engine calls on the blocking pool, so a long
  reindex no longer stalls the async runtime.
- Rust brace imports (`use a::{B, C}`) are expanded into one import per item,
  and `get_dependents` no longer issues one query per importer.
- `semantiq update`: a failed Windows install restores the previous binary;
  `--force` never downgrades a newer build; pre-release versions compare
  correctly (`1.0.1` > `1.0.1-beta`).
- The update notice points at `semantiq update` instead of `npm install -g`,
  and the `semantiq_search` description states the real default `min_score`
  (0.3).
- npm: Windows install ran the `/bin/sh` placeholder instead of `semantiq.exe`.
  The `bin` entry is now a Node launcher, so npm's `.cmd`/`.ps1` shims work in
  PowerShell, cmd and VS Code (#16).

### Security
- Bump `rustls` 0.23.36 -> 0.23.45 and `rustls-webpki` 0.103.13 -> 0.103.15.

## [0.9.0] - 2026-05-29

Reliability, supply-chain, and search-quality release. Re-indexing no longer
leaks orphan `chunks_vec` rows (file ids are now stable), embeddings are
genuinely batched, and a new `semantiq update` self-update command ships
SHA256-verified binaries. Two behavior changes to note: HTTP CORS is now
restrictive by default, and `PARSER_VERSION` 7 → 8 triggers a one-time full
reindex on first start after upgrading.

### Added
- **`semantiq update` self-update command**. Downloads the matching release
  archive, verifies its published SHA256 (fail-closed — aborts if the
  checksum is missing or mismatched), and atomically replaces the running
  binary. Supports `--check` (report availability only) and `--force`.
- **Batched embeddings**. `embed_batch` now runs a single padded forward
  pass instead of one inference per chunk. The CLI `index` command and the
  parser pipeline feed embeddings in batches. `EmbeddingModel` exposes
  `is_stub()` and a unified `EMBEDDING_DIMENSION`.
- **Regression and integration tests**: schema v4 → v5 migration tests, an
  `AutoIndexer` integration suite, the `vec_invariant` test pinning the
  no-orphan contract, and reindex/embedding-dimension checks.

### Changed
- **CORS is restrictive by default (behavior change)**. The HTTP API
  (`--http-port`) no longer allows cross-origin requests out of the box;
  cross-origin access must be opted into explicitly via `--cors-origin`.
  Same-origin clients are unaffected.
- **npm installer verifies the published SHA256 before extraction
  (fail-closed)**. Releases must now ship a `.sha256` file alongside each
  archive or installation aborts. The out-of-lockfile `ort` prebuilt
  runtime is now documented.
- **`PARSER_VERSION` 7 → 8** — triggers a one-time full reindex on the first
  start after upgrading.
- **MCP/HTTP serving hardening**. The auto-indexer's `process_events` now
  runs on `spawn_blocking`, the HTTP server shuts down gracefully, and the
  dead `tools` module was removed.

### Fixed
- **`chunks_vec` orphan rows on reindex (HIGH-1)**. File rows are now kept
  stable across reindexes via `INSERT ... ON CONFLICT(path) DO UPDATE`
  instead of delete-then-reinsert, so reusing the same file id no longer
  strands the previous chunk vectors. `update_chunk_embedding` is now
  transactional with a dimension check, the `foreign_keys` PRAGMA is enabled
  on in-memory connections, and FTS5 results use `ORDER BY rank`.
- **Resilient incremental indexing**. Exclusion is now checked against the
  relative path, the file hash is committed last (so an interrupted index no
  longer marks a file as up-to-date), and lexical paths normalize `..`
  components.
- **Resilient per-file CLI `index`**. A single file error no longer aborts
  the whole run; failures are collected and reported in an error summary.
  Embeddings are generated in batches.
- **Search ranking quality**. Semantic search is skipped on the stub model,
  expanded-variant FTS matches are deduplicated with a penalty, cross-strategy
  scores are min-max normalized, and the min-score floor is aligned. Removed
  N+1 path/language lookups, the file-list cache is now `Arc`'d, text matchers
  are compiled once, and per-file read size is capped.
- **Parser extraction**. Every declarator in a multi-declarator statement is
  now indexed (dedup key includes the name range), Scala multi-binding
  `val`/`var` are handled, doc-comments break across blank lines, text is read
  via `utf8_text()` instead of manual byte slicing, and symbol/import kinds
  gain `Display`/`FromStr`.

### Security
- **Bump `openssl` 0.10.79 → 0.10.80** (GHSA-phqj-4mhp-q6mq, transitive
  dependency) — fixes a potential out-of-bounds write in AES-KW-PAD ciphers.
  Semantiq does not exercise that code path; updated as a precaution.

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
