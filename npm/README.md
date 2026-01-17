# Semantiq MCP

**One MCP Server. Every AI Coding Tool.**

Semantiq gives every AI coding assistant semantic understanding of your codebase. Install once, works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any MCP-compatible tool.

## Installation

```bash
npm install -g semantiq-mcp
```

## Setup

Add to your MCP config (Claude Code, Cursor, etc.):

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

## Supported Languages

- Rust
- TypeScript / JavaScript
- Python
- Go
- Java
- C / C++
- PHP

## Compatibility

Works with all MCP-compatible tools:
- Claude Code
- Cursor
- Windsurf
- GitHub Copilot
- JetBrains IDEs (2025.2+)
- VS Code
- Codex CLI / Aider

## Links

- [GitHub Repository](https://github.com/so-keyldzn/semantiq)
- [Report Issues](https://github.com/so-keyldzn/semantiq/issues)

## License

MIT
