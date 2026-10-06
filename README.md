# Semantiq

**One MCP Server. Every AI Coding Tool.**

Semantiq gives every AI coding assistant semantic understanding of your codebase. Install once, works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any MCP-compatible tool.

## Features

- **3 Search Strategies** (fused in `semantiq_search`): Semantic (embeddings) + Symbol (FTS5) + Lexical (ripgrep)
- **Dependency Graph Analysis**: separate `semantiq_deps` tool (imports + dependents)
- **Structural Intelligence**: real references (`semantiq_find_refs`), change impact (`semantiq_impact`), call graph (`semantiq_calls`), type hierarchy (`semantiq_hierarchy`) and dead code (`semantiq_dead_code`), all from the syntax tree
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
- Installs the `semantiq` [Agent Skill](#use-with-claude-code--agents) in `.claude/skills/semantiq/`
- Creates (or merges) `.mcp.json` at the project root with the `semantiq` MCP server entry
- Adds a short Semantiq section to `CLAUDE.md` pointing at the CLI/skill and the MCP tools
- Updates `.gitignore` to exclude `.semantiq.db*`
- Indexes your entire project with embeddings

Restart Claude Code and you're ready to go! `semantiq init --no-mcp` installs
the skill only, `--no-skill` the MCP server only.

The indexing step of `init` is optional: `semantiq serve` indexes the project
in the background on startup (see [Auto-Indexing](#auto-indexing)). Running
`init` just makes the index ready before the first query.

### Claude Code plugin

Prefer installing from inside Claude Code? The repository is also a plugin
marketplace: the plugin bundles the `semantiq` skill and the MCP server
(`semantiq serve --project ${CLAUDE_PROJECT_DIR}`).

```
/plugin marketplace add so-keyldzn/semantiq
/plugin install semantiq@semantiq
```

The plugin calls the `semantiq` binary, so install it first (see
[Installation](#installation)). Plugin skills are namespaced: the skill shows
up as `semantiq:semantiq`, separate from a project-level skill installed by
`semantiq init`. Use one or the other, not both.

### For Cursor / VS Code

```bash
semantiq init-cursor
```

Creates `.cursor/` and `.vscode/` configurations with MCP server setup.

## Use with Claude Code / agents

Every Semantiq capability is a CLI command that agents can run through their
shell tool. Results go to stdout (compact text by default: one line per
result, each path written once; `--json` for the exact structured output of
the matching MCP tool), logs to stderr.

```bash
semantiq search "where are file renames handled"   # find code by concept
semantiq refs should_exclude_path                   # definitions + usages (AST)
semantiq impact should_exclude_path                 # what breaks + tests to run
semantiq explain RetrievalEngine                    # signature, docs, usages
semantiq deps src/server.rs --json                  # imports / imported by
semantiq calls index_file --direction callers       # who calls it (AST call graph)
semantiq hierarchy EmbeddingModel                   # supertypes + implementors
semantiq dead-code --path-prefix src/               # functions/types nothing uses
semantiq map --focus src/auth/                      # ranked repository overview
```

The **`semantiq` skill** (`skills/semantiq/SKILL.md`, installed by
`semantiq init` into `.claude/skills/semantiq/`) teaches Claude Code when to
reach for these commands instead of grep and how to read their output. Unlike
MCP tool definitions, which are loaded into every session, a skill only enters
the context when it is relevant. Install it for all your projects with
`semantiq init --global` (`~/.claude/skills/semantiq/`); other agents
(Cursor, Codex…) get a pointer in `AGENTS.md` from `semantiq init-cursor`.

Query commands find `.semantiq.db` in the current directory or a parent and
reindex files changed since the last run before answering (`--no-refresh`
skips that check). Without an index they exit with code 1 and suggest
`semantiq index`; usage errors exit with 2.

**Latency.** `refs`, `deps`, `explain`, `impact`, `calls`, `hierarchy`,
`dead-code` and `map` don't load the embedding model and answer in ~20 ms on this repository. `search` loads the ONNX model on
every call: ~0.75–1 s in total, of which ~0.4 s is the model checksum check
and ~0.3 s the ONNX session, the query itself taking ~20 ms. That stays under
a second, so there is no daemon; run `semantiq serve` (MCP) if you need
warm semantic search.

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
semantiq init              # Current directory: skill + MCP + CLAUDE.md + index
semantiq init /my/project  # Specific path
semantiq init --no-mcp     # Skill + CLI only (no .mcp.json entry)
semantiq init --no-skill   # MCP only
semantiq init --no-index   # Skip the initial indexing
semantiq init --force      # Replace skill files you modified with the bundled ones
semantiq init --global     # Install the skill in ~/.claude/skills/semantiq/ only
```

Re-running `init` is safe: unchanged files are left alone, the Semantiq section
of `CLAUDE.md` (between `<!-- semantiq:start -->` markers) is refreshed in
place, and an edited `SKILL.md` is kept unless `--force`.

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
- `AGENTS.md` - Semantiq section pointing terminal agents at the CLI

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

Search code by meaning, symbol name or text (same as `semantiq_search`).

```bash
semantiq search "authentication handler"
semantiq search "db connection" --limit 20
semantiq search "error" --min-score 0.5
semantiq search "api" --file-type rs,ts,py
semantiq search "handler" --symbol-kind function,method
semantiq search "retry with backoff" --snippets   # code of each hit, not just one line
```

Each hit is `path:start-end kind name (score)` followed by its most relevant
line (cut to 120 characters); `(more exist: raise limit)` flags a truncated
list.

Options:
- `--limit N` - Maximum results (default: 10)
- `--min-score F` - Minimum score threshold 0.0-1.0 (default: 0.3)
- `--file-type CSV` - Filter by extensions (e.g., `rs,ts,py`)
- `--symbol-kind CSV` - Filter by symbol types (e.g., `function,method,class`)
- `--snippets` - Print each hit's code instead of one preview line

### `semantiq refs | deps | explain | impact`

The other MCP tools as commands (`semantiq_find_refs`, `semantiq_deps`,
`semantiq_explain`, `semantiq_impact`):

```bash
semantiq refs <SYMBOL> [--limit 30]
semantiq deps <FILE>
semantiq explain <SYMBOL>
semantiq impact <SYMBOL> [--max-depth 2] [--file <FILE>] [--limit 200]
```

All query commands accept `--json`, `--no-refresh`, `--database <FILE>` and
`--project <DIR>`.

### `semantiq calls | hierarchy | dead-code`

The structural tools as commands (`semantiq_calls`, `semantiq_hierarchy`,
`semantiq_dead_code`), with the same parameters:

```bash
semantiq calls <SYMBOL> [--direction callers|callees|both] [--max-depth 1] [--file <FILE>] [--limit 100]
semantiq hierarchy <TYPE> [--max-depth 3] [--limit 200]
semantiq dead-code [--path-prefix src/] [--language rust] [--include-public] [--limit 100]
```

### `semantiq map [OPTIONS]`

Print a ranked map of the repository: its most important files and the
signatures of their key symbols, within a token budget.

```bash
semantiq map
semantiq map --max-tokens 800
semantiq map --focus src/auth/login.rs --focus SessionStore
semantiq map --path-prefix crates/core/
```

Options:
- `--max-tokens N` - Token budget, estimated as characters / 4 (default: 1500, range 256-8000)
- `--focus LIST` - Files, directories or symbol names to center the map on (repeatable or comma-separated)
- `--path-prefix P` - Only list files under this path

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

Semantic + lexical code search combining 3 strategies. Returns, per hit,
`path:lines`, the symbol and one preview line; `snippets: true` adds the code.

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `query` | string | required | Search query (max 500 chars) |
| `limit` | number | 10 | Maximum results |
| `min_score` | number | 0.3 | Score threshold (0.0-1.0) |
| `file_type` | string | - | Filter by extensions (CSV: `rs,ts,py`) |
| `symbol_kind` | string | - | Filter by symbol type (CSV) |
| `snippets` | boolean | false | Include each hit's code (default: one preview line) |

**Symbol kinds:** `function`, `method`, `class`, `struct`, `enum`, `interface`, `trait`, `module`, `variable`, `constant`, `type`

### `semantiq_repo_map`

Compact overview of the repository, meant to be called first on an unfamiliar
codebase. Files are ranked with PageRank over the reference graph (AST
references and resolved imports, as in Aider's repo map), then each listed file
shows the signatures and first doc line of its most used symbols, as many as fit
in the token budget. No LLM call; the same index always gives the same map, and
the unfocused ranking is cached until the index changes.

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `max_tokens` | number | 1500 | Token budget (256-8000, estimated as chars / 4) |
| `focus` | string[] | - | Files, directories or symbol names to center the map on (personalized PageRank) |
| `path_prefix` | string | - | Only list files under this path |

Also available as `semantiq map` and `POST /map` in HTTP mode.

### `semantiq_find_refs`

Find all references (definitions + usages) of a symbol.

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `symbol` | string | required | Symbol name to search |
| `limit` | number | 30 | Maximum results |

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

### `semantiq_impact`

List what may break if a function, method or type changes: every place that
uses it, then the users of those places, grouped by file, with the test files
to run. Each site has a confidence (`same_file`, `imports`, `unique_name`,
`name_only`) since matching is by name.

**Parameters:** `symbol` (required), `file_path` (pick one of several
definitions), `max_depth` (default 2, max 4), `limit` (default 200).

### `semantiq_calls`

Call graph of a function or method: who calls it (`callers`) and what it calls
(`callees`), up to `max_depth` levels. Each edge names the enclosing caller;
comments, strings and non-call mentions are ignored. Calls to names the project
does not define are listed in `external_callees`.

**Parameters:**
| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `symbol` | string | required | Function or method name |
| `direction` | string | `both` | `callers`, `callees` or `both` |
| `file_path` | string | - | Restrict to the definition in this file |
| `max_depth` | number | 1 | Call levels to follow (max 3) |
| `limit` | number | 100 | Maximum call edges (max 1000) |

### `semantiq_hierarchy`

Supertypes and subtypes / implementors of a class, interface or trait,
transitively: Rust `impl Trait for Type` and supertraits, TS/JS, Python, Java,
Kotlin, C#, C++, PHP, Ruby, Scala (not Go's implicit interfaces).

**Parameters:** `symbol` (required), `max_depth` (default 3, max 5), `limit`
(per direction, default 200).

### `semantiq_dead_code`

Functions, methods and types that nothing references outside their own
definition. Entry points, tests, trait / interface members and (unless
`include_public`) public symbols are excluded; each result has a confidence
(`high`, `medium`, `low`) and the reasons. Matching is by name: confirm before
deleting.

**Parameters:** `path_prefix`, `language`, `include_public` (default false),
`limit` (default 100, max 1000).

`semantiq_calls`, `semantiq_hierarchy` and `semantiq_dead_code` are also
available as `semantiq calls`, `semantiq hierarchy`, `semantiq dead-code` and,
in HTTP mode, `POST /calls`, `POST /hierarchy`, `POST /dead-code`.

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

## Benchmark

`bench/agent/` measures Claude Code on read-only code-navigation tasks with and
without Semantiq (Python 3 stdlib harness, headless `claude -p`, throw-away
checkouts, automatic precision/recall scoring):

```bash
python3 bench/agent/run.py --dry-run          # list planned runs and cost estimate
python3 bench/agent/run.py --build --jobs 4   # baseline vs semantiq MCP
```

The first run is in
[`bench/agent/results/2026-10-06/`](bench/agent/results/2026-10-06/report.md):
25 tasks on semantiq and ripgrep, 3 repetitions, Sonnet. On these medium-sized
repositories grep already answers 96% of the questions, and Semantiq adds no
measurable accuracy. The MCP tool definitions add about 1.6k tokens per request
(+22% input tokens, +12% cost), and they are rarely called unless the agent is
told to use them. See [`bench/agent/README.md`](bench/agent/README.md) for the
method.

## Documentation

- **[MCP Setup Guide](docs/MCP-SETUP-GUIDE.md)** - Detailed configuration for all IDEs
- **[CHANGELOG.md](CHANGELOG.md)** - Version history

## License

MIT
