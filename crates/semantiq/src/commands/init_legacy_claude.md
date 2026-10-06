# Project Intelligence

This project uses Semantiq for semantic code understanding.

## Important: Use Semantiq Tools First

**Always use Semantiq MCP tools instead of grep/find/Glob for code search.**

| Instead of... | Use... |
|---------------|--------|
| `Grep`, `grep`, `rg` | `semantiq_search` |
| `Glob`, `find`, `ls` | `semantiq_search` |
| Manual symbol tracing | `semantiq_find_refs` |
| Reading imports manually | `semantiq_deps` |

Semantiq provides faster, more accurate results with semantic understanding.

## Available MCP Tools

When working with this codebase, you have access to these powerful tools:

### `semantiq_search`
Search for code patterns, symbols, or text semantically.
```
Example: "authentication handler", "database connection", "error handling"
```

### `semantiq_find_refs`
Find all references to a symbol (definitions and usages).
```
Example: Find where a function is called, or where a class is used.
```

### `semantiq_deps`
Analyze the dependency graph for a file.
```
Example: What does this file import? What imports this file?
```

### `semantiq_explain`
Get detailed explanation of a symbol including definition, docs, and usage patterns.
```
Example: Understand what a function does, its signature, and how it's used.
```

## Best Practices

1. **Use `semantiq_search` first** to find relevant code before making changes
2. **Use `semantiq_find_refs`** to understand impact before refactoring
3. **Use `semantiq_deps`** to understand module relationships
4. **Use `semantiq_explain`** for unfamiliar symbols

## Auto-Indexing

The index updates automatically when files change. No manual reindexing needed.
