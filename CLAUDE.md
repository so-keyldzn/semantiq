# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Build the project
cargo build

# Build release version
cargo build --release

# Run tests
cargo test

# Run tests for a specific crate
cargo test -p semantiq-parser

# Run a single test
cargo test -p semantiq-parser test_language_from_extension

# Check code without building
cargo check

# Format code
cargo fmt

# Lint code
cargo clippy
```

## CLI Usage

```bash
# Index a project
cargo run -- index /path/to/project

# Start MCP server
cargo run -- serve --project /path/to/project

# Search (for testing)
cargo run -- search "query"

# Show index stats
cargo run -- stats
```

## Architecture

Semantiq is a Rust workspace providing semantic code understanding for AI coding assistants via MCP (Model Context Protocol).

### Crate Structure

```
crates/
├── semantiq/           # Main binary (CLI entry point)
├── semantiq-mcp/       # MCP server implementation (rmcp)
├── semantiq-parser/    # Tree-sitter parsing, symbol/chunk/import extraction
├── semantiq-index/     # SQLite storage (rusqlite), file/symbol/chunk records
├── semantiq-retrieval/ # Search engine, query expansion, result ranking
└── semantiq-embeddings/# Embedding model (placeholder for semantic search)
```

### Data Flow

1. **Indexing**: `semantiq index` walks project files → `semantiq-parser` extracts symbols/chunks/imports using tree-sitter → `semantiq-index` stores in SQLite (`.semantiq.db`)

2. **Serving**: `semantiq serve` starts MCP server on stdio → tools call `semantiq-retrieval` → queries `semantiq-index`

### MCP Tools

- `semantiq_search` - Semantic + lexical code search
- `semantiq_find_refs` - Find all references to a symbol
- `semantiq_deps` - Analyze file dependency graph
- `semantiq_explain` - Get detailed symbol explanation

### Supported Languages

Rust, TypeScript, JavaScript, Python, Go, Java, C, C++, PHP - all via tree-sitter grammars.

### Key Types

- `Language` / `LanguageSupport` - Multi-language tree-sitter parsing (`semantiq-parser/src/language.rs`)
- `IndexStore` - SQLite wrapper with FTS5 search (`semantiq-index/src/store.rs`)
- `RetrievalEngine` - Query execution and result ranking (`semantiq-retrieval/src/engine.rs`)
- `SemantiqServer` - MCP server with tool handlers (`semantiq-mcp/src/server.rs`)
