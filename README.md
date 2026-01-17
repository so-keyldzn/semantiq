# Semantiq

**One MCP Server. Every AI Coding Tool.**

Semantiq gives every AI coding assistant semantic understanding of your codebase. Install once, works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any MCP-compatible tool.

## Installation

```bash
# npm
npm install -g semantiq-mcp

# Cargo (from source)
cargo install --git https://github.com/so-keyldzn/semantiq.git
```

## Quick Start (10 seconds)

```bash
cd /path/to/your/project
semantiq init
```

This automatically:
- Creates `.claude/settings.json` with MCP configuration
- Creates `CLAUDE.md` with tool instructions
- Updates `.gitignore` to exclude the index database
- Indexes your entire project

Restart Claude Code and you're ready to go!

## Manual Setup

If you prefer manual configuration, add to your MCP config:

```json
{
  "mcpServers": {
    "semantiq": {
      "command": "semantiq",
      "args": ["serve"]
    }
  }
}
```

## Auto-Indexing

Semantiq automatically watches your project for file changes and updates the index in real-time. No manual reindexing needed.

## MCP Tools

| Tool | Description |
|------|-------------|
| `semantiq_search` | Semantic + lexical code search |
| `semantiq_find_refs` | Find all references to a symbol |
| `semantiq_deps` | Analyze dependency graph |
| `semantiq_explain` | Get detailed symbol explanations |

## CLI Commands

```bash
# Initialize Semantiq for a project (recommended)
semantiq init

# Index a project manually
semantiq index /path/to/project

# Start MCP server
semantiq serve --project /path/to/project

# Search (for testing)
semantiq search "authentication handler"

# Show index stats
semantiq stats
```

## Compatibility

Works with all MCP-compatible tools:
- Claude Code
- Cursor
- Windsurf
- GitHub Copilot
- JetBrains IDEs (2025.2+)
- VS Code
- Codex CLI / Aider

## License

MIT
