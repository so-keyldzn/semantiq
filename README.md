# Semantiq

**One MCP Server. Every AI Coding Tool.**

Semantiq gives every AI coding assistant semantic understanding of your codebase. Install once, works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any MCP-compatible tool.

## Features

- **4 Search Strategies**: Semantic (embeddings) + Lexical (ripgrep) + Symbol (FTS5) + Dependency graph
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

## Quick Start

```bash
cd /path/to/your/project
semantiq init
```

This automatically:
- Creates `.claude/settings.json` with MCP configuration
- Creates `CLAUDE.md` with tool usage instructions
- Updates `.gitignore` to exclude `.semantiq.db`
- Indexes your entire project with embeddings

Restart Claude Code and you're ready to go!

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
| JavaScript | `.js`, `.jsx`, `.mjs` |
| Python | `.py`, `.pyi` |
| Go | `.go` |
| Java | `.java` |
| C | `.c`, `.h` |
| C++ | `.cpp`, `.cc`, `.hpp` |
| PHP | `.php`, `.phtml` |
| Ruby | `.rb`, `.rake` |
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
└── semantiq-embeddings/# ONNX model (MiniLM-L6-v2, 384-D)
```

**Data Flow:**
1. Parse source files with tree-sitter
2. Extract symbols, chunks, and imports
3. Generate embeddings (384-D vectors)
4. Store in SQLite with FTS5 + vector search
5. Query via MCP tools with multi-strategy fusion

## Compatibility

Works with all MCP-compatible tools:

| Tool | Config Location |
|------|-----------------|
| Claude Code (CLI) | `.claude/settings.json` |
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
// .claude/settings.json
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

No manual reindexing required for normal development.

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
- **Embedding model**: Downloaded automatically on first run (~90MB from HuggingFace). Stored in:
  - macOS: `~/Library/Application Support/semantiq/models/`
  - Linux: `~/.local/share/semantiq/models/`
  - Windows: `%APPDATA%\semantiq\models\`
- **macOS Intel (x86_64)**: Not supported due to ONNX Runtime limitation.
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
