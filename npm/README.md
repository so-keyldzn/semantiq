# Semantiq MCP

**One MCP Server. Every AI Coding Tool.**

Semantiq gives every AI coding assistant semantic understanding of your codebase. Install once, works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any MCP-compatible tool.

## Installation

```bash
npm install -g semantiq-mcp
```

## Quick Start (10 seconds)

```bash
cd /path/to/your/project
semantiq init
```

This automatically:
- Installs the `semantiq` Agent Skill in `.claude/skills/semantiq/`
- Creates (or merges) `.mcp.json` with the MCP server entry
- Adds a short Semantiq section to `CLAUDE.md` (and `.gitignore`s the index)
- Indexes your entire project

Restart Claude Code and you're ready to go! Variants:

```bash
semantiq init --global     # install the skill once for all projects (~/.claude/skills/semantiq/)
semantiq init --no-mcp     # skill + CLI only, no MCP server
semantiq init --no-skill   # MCP server only
```

Indexing up front is optional: `semantiq serve` indexes the project in the
background on startup and keeps it up to date as files change.

## Agent Skill

The `semantiq` skill teaches Claude Code when to call the `semantiq` CLI
through Bash instead of grep: who calls a function, what implements a trait,
where a symbol is really used, what breaks before a change, unused code, and
conceptual questions with no keyword to grep. Unlike MCP tool definitions,
which are loaded into every session, a skill only enters the context when it
is relevant. It needs the `semantiq` binary installed by this package.

## Claude Code plugin

Instead of `semantiq init`, you can install the skill and the MCP server from
inside Claude Code:

```
/plugin marketplace add so-keyldzn/semantiq
/plugin install semantiq@semantiq
```

The plugin calls the `semantiq` binary, so keep this npm package installed.
Its skill shows up as `semantiq:semantiq`; use either the plugin or `semantiq init`, not both.

## Manual Setup

If you prefer manual configuration, add to your MCP config (Claude Code, Cursor, etc.):

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

## Tools

Every capability is available as an MCP tool, a CLI command (add `--json` for
the same structured output) and an HTTP endpoint (`semantiq serve --http-port`).

| MCP tool | CLI | Description |
|------|------|-------------|
| `semantiq_search` | `semantiq search "<query>"` | Semantic + symbol + text code search |
| `semantiq_repo_map` | `semantiq map` | Ranked map of the key files and symbols |
| `semantiq_find_refs` | `semantiq refs <symbol>` | Definitions and real usages (from the syntax tree) |
| `semantiq_deps` | `semantiq deps <file>` | What a file imports and who imports it |
| `semantiq_explain` | `semantiq explain <symbol>` | Signature, docs and usage of a symbol |
| `semantiq_impact` | `semantiq impact <symbol>` | What may break if a symbol changes, tests to run |
| `semantiq_calls` | `semantiq calls <symbol>` | Callers and callees |
| `semantiq_hierarchy` | `semantiq hierarchy <type>` | Supertypes and implementers / subtypes |
| `semantiq_dead_code` | `semantiq dead-code` | Symbols with no reference |

## Other commands

```bash
semantiq index /path/to/project   # index manually (optional: serve indexes automatically)
semantiq serve --project .        # MCP server (stdio)
semantiq stats                    # index statistics
semantiq update                   # self-update to the latest release
```

The first run downloads the code embedding model (~139 MB, CodeRankEmbed,
SHA-256 verified). Set `SEMANTIQ_EMBEDDINGS=stub` to skip it (semantic search
disabled).

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
- [Changelog](https://github.com/so-keyldzn/semantiq/blob/main/CHANGELOG.md)
- [Report Issues](https://github.com/so-keyldzn/semantiq/issues)

## License

MIT
