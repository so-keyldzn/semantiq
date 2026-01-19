//! Initialize Semantiq for a project (creates .claude/ config and indexes)

use anyhow::Result;
use std::fs;
use std::path::Path;

use super::common::resolve_project_root;
use super::index::index;

pub async fn init(path: &Path) -> Result<()> {
    let project_root = resolve_project_root(path)?;

    println!("Initializing Semantiq for {:?}", project_root);

    // 1. Create .claude directory
    let claude_dir = project_root.join(".claude");
    if !claude_dir.exists() {
        fs::create_dir_all(&claude_dir)?;
        println!("Created .claude/");
    }

    // 2. Create .claude/settings.json with MCP config
    let settings_path = claude_dir.join("settings.json");
    let settings_content = r#"{
  "mcpServers": {
    "semantiq": {
      "command": "semantiq",
      "args": ["serve"]
    }
  }
}
"#;
    fs::write(&settings_path, settings_content)?;
    println!("Created .claude/settings.json");

    // 3. Create CLAUDE.md with instructions
    let claude_md_path = project_root.join("CLAUDE.md");
    let claude_md_content = r#"# Project Intelligence

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
"#;

    if !claude_md_path.exists() {
        fs::write(&claude_md_path, claude_md_content)?;
        println!("Created CLAUDE.md");
    } else {
        println!("CLAUDE.md already exists, skipping");
    }

    // 4. Update .gitignore
    let gitignore_path = project_root.join(".gitignore");
    let gitignore_entry = ".semantiq.db";

    if gitignore_path.exists() {
        let content = fs::read_to_string(&gitignore_path)?;
        if !content.contains(gitignore_entry) {
            let mut file = fs::OpenOptions::new().append(true).open(&gitignore_path)?;
            use std::io::Write;
            writeln!(file, "\n# Semantiq\n{}", gitignore_entry)?;
            println!("Added .semantiq.db to .gitignore");
        }
    } else {
        fs::write(
            &gitignore_path,
            format!("# Semantiq\n{}\n", gitignore_entry),
        )?;
        println!("Created .gitignore");
    }

    // 5. Index the project
    println!("\nIndexing project...");
    index(path, None, false).await?;

    println!("\n✓ Semantiq initialized successfully!");
    println!("\nNext steps:");
    println!("  1. Restart Claude Code to load the MCP server");
    println!("  2. The semantiq tools will be available automatically");

    Ok(())
}
