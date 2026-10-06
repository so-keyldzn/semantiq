# Semantiq

**One MCP Server. Every AI Coding Tool.**

Semantiq gives every AI coding assistant semantic understanding of your codebase. Install once, works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any MCP-compatible tool.

## Features

- **3 Search Strategies** (fused in `semantiq_search`): Semantic (embeddings) + Symbol (FTS5) + Lexical (ripgrep)
- **Dependency Graph Analysis**: separate `semantiq_deps` tool (imports + dependents)
- **19 Languages**: Full tree-sitter parsing support
- **Auto-Indexing**: Real-time file watching, no manual reindex needed
- **Smart Query Expansion**: Automatic case conversion (`camelCase` ↔ `snake_case`)
- **Secure**: Path traversal protection, SQL injection prevention, DoS safeguards

## Installation

```bash
# npm (recommended)
npm install -g semantiq-mcp

# Cargo (from source)
cargo install --git https://github.com/so-keyldzn/semantiq.git
```

### Embedding model (first run)

Semantic search uses [CodeRankEmbed](https://huggingface.co/nomic-ai/CodeRankEmbed)
(INT8 ONNX export, 768-D), enabled by default in the npm binaries and in
`cargo install` / `cargo build`.

- **Download**: on first `index` / `serve`, about **140 MB** (model ~139 MB +
  tokenizer ~0.7 MB) is fetched once from HuggingFace and verified against
  pinned SHA-256 digests. The build itself also downloads the ONNX Runtime
  library (`ort`).
- **Cache**: `~/Library/Application Support/semantiq/models/` (macOS),
  `~/.local/share/semantiq/models/` (Linux), `%APPDATA%\semantiq\models\`
  (Windows). Delete it to force a re-download.
- **Indexing time**: the first full index embeds every chunk; as a reference, this repository (~110 files, ~31k lines, ~450 chunks) takes about 2 minutes on an Apple M1 Max (8 ONNX threads, see `SEMANTIQ_ONNX_THREADS`). Later runs only reindex changed files.
- **Opting out**: `SEMANTIQ_EMBEDDINGS=stub` skips the model (no download);
  symbol and text search still work, semantic search is disabled. To build
  without ONNX Runtime at all: `cargo install --git
  https://github.com/so-keyldzn/semantiq.git --no-default-features`.
- **macOS Intel (x86_64)**: ONNX Runtime has no prebuilt binary for this
  target, so its release is built without embeddings. `semantiq serve` logs a
  warning, `semantiq stats` / `GET /stats` report semantic search as
  unavailable, and search falls back to symbols and text.

## Quick Start

```bash
cd /path/to/your/project
semantiq init
```

This automatically:
- Creates (or merges) `.mcp.json` at the project root with the `semantiq` MCP server entry
- Creates `CLAUDE.md` with tool usage instructions
- Updates `.gitignore` to exclude `.semantiq.db`
- Indexes your entire project with embeddings

Restart Claude Code and you're ready to go!

The indexing step of `init` is optional: `semantiq serve` indexes the project
in the background on startup (see [Auto-Indexing](#auto-indexing)). Running
`init` just makes the index ready before the first query.

### For Cursor / VS Code

```bash
semantiq init-cursor
```

Creates `.cursor/` and `.vscode/` configurations with MCP server setup.

## Manual Setup

If you prefer manual configuration, add to your MCP config:

```json
{
  "mcpServers": {
    "semantiq": {
      "command": "semantiq",
      "args": ["serve", "--project", "."]
    }
  }
}
```

## CLI Commands

### `semantiq init [PATH]`

Initialize Semantiq for a project (recommended first step).

```bash
semantiq init              # Current directory
semantiq init /my/project  # Specific path
```

### `semantiq init-cursor [PATH]`

Setup Cursor and VS Code configuration files.

```bash
semantiq init-cursor
```

Creates:
- `.cursor/rules/project.mdc` - Project guidelines
- `.cursor/rules/semantiq.mdc` - MCP tools usage
- `.cursor/mcp.json` - MCP server config
- `.cursorignore` - Indexing exclusions
- `.vscode/settings.json`, `tasks.json`, `launch.json`, `extensions.json`

### `semantiq serve [OPTIONS]`

Start the MCP server.

```bash
semantiq serve                           # Use current directory
semantiq serve --project /path/to/project
semantiq serve --database /custom/path.db
semantiq serve --no-update-check         # Disable version notifications
```

### `semantiq update [OPTIONS]`

Self-update to the latest release. The downloaded archive is verified by SHA256 checksum before it replaces the current binary.

```bash
semantiq update          # Download and install the latest version
semantiq update --check  # Check for a newer version without installing
```

### `semantiq index [PATH] [OPTIONS]`

Manually index a project.

```bash
semantiq index                   # Index current directory
semantiq index /path/to/project
semantiq index --force           # Force full reindex (ignore cache)
semantiq index --database /path  # Custom database location
```

### `semantiq search <QUERY> [OPTIONS]`

Search from the command line (useful for testing).

```bash
semantiq search "authentication handler"
semantiq search "db connection" --limit 20
semantiq search "error" --min-score 0.5
semantiq search "api" --file-type rs,ts,py
semantiq search "handler" --symbol-kind function,method
```

Options:
- `--limit N` - Maximum results (default: 10)
- `--min-score F` - Minimum score threshold 0.0-1.0 (default: 0.35)
- `--file-type CSV` - Filter by extensions (e.g., `rs,ts,py`)
- `--symbol-kind CSV` - Filter by symbol types (e.g., `function,method,class`)

### `semantiq stats`

Display index statistics.

```bash
semantiq stats
semantiq stats --database /custom/path.db
```

Output:
```
Semantiq Index Statistics
========================
Database: /path/to/.semantiq.db
Files indexed: 26
Symbols: 313
Chunks: 85
Dependencies: 142
```

## MCP Tools

### `semantiq_search`

Semantic + lexical code search combining 4 strategies.

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `query` | string | required | Search query (max 500 chars) |
| `limit` | number | 20 | Maximum results |
| `min_score` | number | 0.35 | Score threshold (0.0-1.0) |
| `file_type` | string | - | Filter by extensions (CSV: `rs,ts,py`) |
| `symbol_kind` | string | - | Filter by symbol type (CSV) |

**Symbol kinds:** `function`, `method`, `class`, `struct`, `enum`, `interface`, `trait`, `module`, `variable`, `constant`, `type`

### `semantiq_find_refs`

Find all references (definitions + usages) of a symbol.

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `symbol` | string | required | Symbol name to search |
| `limit` | number | 50 | Maximum results |

### `semantiq_deps`

Analyze dependency graph (imports and dependents).

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `file_path` | string | required | File to analyze |

Returns:
- **Imports**: What this file depends on
- **Imported by**: Files that depend on this file

### `semantiq_explain`

Get detailed explanation of a symbol.

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `symbol` | string | required | Symbol name to explain |

Returns:
- All definitions found
- Signatures and documentation
- Usage patterns and locations

## Supported Languages

### Full Support (symbols + imports + chunks + embeddings)

| Language | Extensions |
|----------|-----------|
| Rust | `.rs` |
| TypeScript | `.ts`, `.tsx` |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` |
| Python | `.py`, `.pyi` |
| Go | `.go` |
| Java | `.java` |
| C | `.c`, `.h` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`, `.hh` |
| PHP | `.php`, `.phtml`, `.php3`, `.php4`, `.php5`, `.php7`, `.phps` |
| Ruby | `.rb`, `.rake`, `.gemspec` |
| C# | `.cs` |
| Kotlin | `.kt`, `.kts` |
| Scala | `.scala`, `.sc` |
| Bash | `.sh`, `.bash`, `.zsh` |
| Elixir | `.ex`, `.exs` |

### Partial Support (chunks + embeddings only)

| Language | Extensions |
|----------|-----------|
| HTML | `.html`, `.htm` |
| JSON | `.json` |
| YAML | `.yaml`, `.yml` |
| TOML | `.toml` |

## Architecture

```
crates/
├── semantiq/           # CLI binary (clap subcommands)
├── semantiq-mcp/       # MCP server (rmcp, 4 tools)
├── semantiq-parser/    # Tree-sitter parsing (19 languages)
├── semantiq-index/     # SQLite storage (FTS5, sqlite-vec)
├── semantiq-retrieval/ # Search engine (4 strategies)
└── semantiq-embeddings/# ONNX model (CodeRankEmbed, 768-D)
```

**Data Flow:**
1. Parse source files with tree-sitter
2. Extract symbols, chunks, and imports
3. Generate embeddings (768-D vectors)
4. Store in SQLite with FTS5 + vector search
5. Query via MCP tools with multi-strategy fusion

## Compatibility

Works with all MCP-compatible tools:

| Tool | Config Location |
|------|-----------------|
| Claude Code (CLI) | `.mcp.json` (project root) — written by `semantiq init` |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Cursor | `.cursor/mcp.json` |
| Windsurf | `.windsurf/mcp.json` |
| VS Code + Continue | `~/.continue/config.json` |
| GitHub Copilot | Via MCP proxy |
| JetBrains IDEs | 2025.2+ required |
| Codex CLI / Aider | Standard MCP |

### Configuration Examples

**Claude Code (project-specific):**
```json
// .mcp.json
{
  "mcpServers": {
    "semantiq": {
      "command": "semantiq",
      "args": ["serve", "--project", "."]
    }
  }
}
```

**Claude Desktop (macOS):**
```json
// ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "semantiq": {
      "command": "/usr/local/bin/semantiq",
      "args": ["serve", "--project", "/absolute/path/to/project"]
    }
  }
}
```

**Cursor:**
```json
// .cursor/mcp.json
{
  "mcpServers": {
    "semantiq": {
      "command": "semantiq",
      "args": ["serve", "--project", "."]
    }
  }
}
```

## Auto-Indexing

Semantiq automatically:
- Indexes your project on MCP server startup
- Watches for file changes (2-second intervals)
- Re-indexes modified files incrementally
- Regenerates embeddings as needed

No manual reindexing required for normal development, and `semantiq index` /
`semantiq init` are not required before `serve`.

While the initial pass runs, tool responses start with a
`⏳ Initial indexing in progress` notice (results may be incomplete), and the
HTTP `GET /stats` endpoint reports `"indexing": true`.

The index lives in `.semantiq.db` at the project root, so each git worktree has
its own index and is indexed separately.

### Force Reindex

To force a complete reindex:
```bash
semantiq index --force
```

Automatic reindex is triggered when:
- Parser version changes (new tree-sitter grammars)
- Schema version changes (database migrations)

## Known Limitations

- **`semantiq_explain`**: Works best with functions, classes, structs, and interfaces. Exported variables (e.g., `export const config = {...}`) may not be indexed as symbols. Use `semantiq_search` as a fallback.
- **Embedding model**: CodeRankEmbed INT8, downloaded automatically on first run (~139MB from HuggingFace, SHA-256 pinned). See [Embedding model (first run)](#embedding-model-first-run) for the cache location and opt-out.
- **macOS Intel (x86_64)**: No semantic (embedding) search, due to an ONNX Runtime limitation; symbol and text search work.
- **File size limit**: Files larger than 1MB are skipped.

## Excluded Directories

These directories are automatically excluded from indexing:
```
node_modules, target, dist, build, vendor, .next,
__pycache__, venv, .venv, coverage, .nyc_output,
.git, .hg, .svn, out, .output, .nuxt, .cache,
.parcel-cache, .turbo
```

Hidden directories (starting with `.`) are also excluded.

## Documentation

- **[MCP Setup Guide](docs/MCP-SETUP-GUIDE.md)** - Detailed configuration for all IDEs
- **[CHANGELOG.md](CHANGELOG.md)** - Version history

## License

MIT
