# Semantiq

**One MCP Server. Every AI Coding Tool.**

Semantiq gives every AI coding assistant semantic understanding of your codebase. Install once, works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any MCP-compatible tool.

## Installation

```bash
# Homebrew (macOS/Linux)
brew install nicololau/tap/semantiq

# npm (cross-platform)
npm i -g semantiq

# Cargo (Rust)
cargo install semantiq
```

## Setup (30 seconds)

Add to your MCP config:

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

That's it. Semantiq auto-detects your project and starts indexing.

## MCP Tools

| Tool | Description |
|------|-------------|
| `semantiq_search` | Semantic + lexical code search |
| `semantiq_find_refs` | Find all references to a symbol |
| `semantiq_deps` | Analyze dependency graph |
| `semantiq_explain` | Get detailed symbol explanations |

## CLI Commands

```bash
# Index a project
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
