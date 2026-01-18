# Changelog

All notable changes to Semantiq will be documented in this file.

## [0.2.5] - 2026-01-18

### Added
- Automatic reindexation when parser version changes (no more manual `--force` needed)
- `PARSER_VERSION` constant to track parser logic changes
- Support for `const`/`let` variable extraction in TypeScript/JavaScript
- GitHub Sponsors funding configuration

### Changed
- Version detection uses atomic transactions to prevent race conditions
- Documentation updated with known limitations and setup guides

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
