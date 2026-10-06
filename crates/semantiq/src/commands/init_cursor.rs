//! Initialize Cursor/VS Code configuration for a project

use anyhow::Result;
use std::fs;
use std::path::Path;

use super::common::resolve_project_root;
use super::init::upsert_managed_block;

/// Semantiq section of AGENTS.md, read by Cursor's agent and other coding
/// agents that run shell commands: the CLI works without the MCP server.
const AGENTS_MD_BLOCK: &str = r#"## Semantiq

This project is indexed by Semantiq (`.semantiq.db`). Besides the `semantiq_*`
MCP tools, the `semantiq` CLI answers from a terminal the questions where grep
is guesswork: `semantiq refs <symbol>` (real usages, no comment or string
hits), `semantiq impact <symbol>` (what a change breaks, tests to run),
`semantiq calls <symbol>` (callers and callees), `semantiq hierarchy <type>`
(what implements or extends it), `semantiq dead-code` (unused code),
`semantiq search "<what the code does>"` (code for a concept with no keyword
to grep), `semantiq explain <symbol>`, `semantiq deps <file>`. Add `--json`
for structured output. Keep grep for exact strings, error messages and config
keys."#;

/// Writes content to a file, checking if it already exists.
/// Returns true if the file was written, false if skipped.
fn write_if_not_exists(path: &Path, content: &str, name: &str) -> Result<bool> {
    if path.exists() {
        println!("Skipped {} (already exists)", name);
        Ok(false)
    } else {
        fs::write(path, content)?;
        println!("Created {}", name);
        Ok(true)
    }
}

pub(crate) async fn init_cursor(path: &Path) -> Result<()> {
    let project_root = resolve_project_root(path)?;

    println!("Initializing Cursor/VS Code config for {:?}", project_root);

    // 1. Create .cursor directory structure
    let cursor_dir = project_root.join(".cursor");
    let rules_dir = cursor_dir.join("rules");
    fs::create_dir_all(&rules_dir)?;

    // 2. Create .cursor/rules/project.mdc (general project guidelines)
    let project_rules_content = r#"---
description: General project guidelines
globs:
  - "**/*"
alwaysApply: true
---

# Project Guidelines

## Code Quality

- Write clear, readable code
- Keep functions small and focused
- Use descriptive names for variables and functions
- Add comments only when the code isn't self-explanatory
- Format code consistently

## Before Committing

- Run tests
- Run linter/formatter
- Review your changes

## Best Practices

- Handle errors explicitly
- Write tests for new functionality
- Document public APIs
"#;
    write_if_not_exists(
        &rules_dir.join("project.mdc"),
        project_rules_content,
        ".cursor/rules/project.mdc",
    )?;

    // 3. Create .cursor/rules/semantiq.mdc (MCP tools usage)
    let semantiq_rules_content = r#"---
description: Semantiq MCP tools for semantic code understanding
globs:
  - "**/*"
alwaysApply: true
---

# Semantiq MCP Tools

This project uses Semantiq for semantic code understanding.

## Available Tools

- `semantiq_search` - Search code by concept, symbol name or text
- `semantiq_find_refs` - Find symbol references
- `semantiq_deps` - Analyze dependencies
- `semantiq_explain` - Explain symbols
- `semantiq_impact` - What a change breaks, and the tests to run
- `semantiq_calls` - Callers and callees of a function
- `semantiq_hierarchy` - Supertypes, subtypes and implementors of a type
- `semantiq_dead_code` - Unused functions, methods and types

## When to use them

Prefer Semantiq when the answer depends on the code's structure; keep grep for
exact strings, error messages and config keys, where it is faster.

| Question | Use... |
|----------|--------|
| Where is a symbol really used (not in comments/strings)? | `semantiq_find_refs` |
| What breaks if I change it? | `semantiq_impact` |
| Who calls this function? What does it call? | `semantiq_calls` |
| What implements this interface / extends this class? | `semantiq_hierarchy` |
| Is this code still used? | `semantiq_dead_code` |
| Where is a concept handled, with no keyword to grep? | `semantiq_search` |
| What does this file import / who imports it? | `semantiq_deps` |
"#;
    write_if_not_exists(
        &rules_dir.join("semantiq.mdc"),
        semantiq_rules_content,
        ".cursor/rules/semantiq.mdc",
    )?;

    // 4. Create .cursor/mcp.json (MCP server configuration)
    let mcp_json_content = r#"{
  "mcpServers": {
    "semantiq": {
      "command": "semantiq",
      "args": ["serve"]
    }
  }
}
"#;
    write_if_not_exists(
        &cursor_dir.join("mcp.json"),
        mcp_json_content,
        ".cursor/mcp.json",
    )?;

    // 5. Create .cursorignore
    let cursorignore_content = r#"# Dependencies
node_modules/
vendor/
.venv/
venv/
__pycache__/

# Build artifacts
dist/
build/
target/
out/
.next/
.nuxt/

# Database files
*.db
*.db-wal
*.db-shm
.semantiq.db*

# Version control
.git/

# IDE
.idea/
.vscode/

# Logs and caches
*.log
.cache/
.tmp/

# Package lock files
package-lock.json
yarn.lock
pnpm-lock.yaml
Cargo.lock
poetry.lock
Pipfile.lock
composer.lock
Gemfile.lock

# Environment
.env
.env.*
"#;
    write_if_not_exists(
        &project_root.join(".cursorignore"),
        cursorignore_content,
        ".cursorignore",
    )?;

    // 6. Create .vscode directory
    let vscode_dir = project_root.join(".vscode");
    fs::create_dir_all(&vscode_dir)?;

    // 7. Create .vscode/settings.json
    let settings_json = r#"{
    "editor.tabSize": 4,
    "editor.formatOnSave": true,
    "editor.minimap.enabled": false,
    "editor.bracketPairColorization.enabled": true,
    "editor.guides.bracketPairs": true,
    "files.trimTrailingWhitespace": true,
    "files.insertFinalNewline": true,
    "files.watcherExclude": {
        "**/node_modules/**": true,
        "**/.git/**": true,
        "**/dist/**": true,
        "**/build/**": true,
        "**/target/**": true,
        "**/*.db": true,
        "**/*.db-wal": true,
        "**/*.db-shm": true
    },
    "files.exclude": {
        "**/.git": true,
        "**/node_modules": true,
        "**/.DS_Store": true
    },
    "search.exclude": {
        "**/node_modules": true,
        "**/dist": true,
        "**/build": true,
        "**/target": true,
        "**/*.db": true
    }
}
"#;
    write_if_not_exists(
        &vscode_dir.join("settings.json"),
        settings_json,
        ".vscode/settings.json",
    )?;

    // 8. Create .vscode/tasks.json
    let tasks_json = r#"{
    "version": "2.0.0",
    "tasks": [
        {
            "label": "Semantiq: Index project",
            "type": "shell",
            "command": "semantiq",
            "args": ["index"],
            "problemMatcher": []
        },
        {
            "label": "Semantiq: Start MCP server",
            "type": "shell",
            "command": "semantiq",
            "args": ["serve"],
            "problemMatcher": []
        },
        {
            "label": "Semantiq: Show stats",
            "type": "shell",
            "command": "semantiq",
            "args": ["stats"],
            "problemMatcher": []
        },
        {
            "label": "Semantiq: Search",
            "type": "shell",
            "command": "semantiq",
            "args": ["search", "${input:searchQuery}"],
            "problemMatcher": []
        }
    ],
    "inputs": [
        {
            "id": "searchQuery",
            "type": "promptString",
            "description": "Enter search query"
        }
    ]
}
"#;
    write_if_not_exists(
        &vscode_dir.join("tasks.json"),
        tasks_json,
        ".vscode/tasks.json",
    )?;

    // 9. Create .vscode/launch.json
    let launch_json = r#"{
    "version": "0.2.0",
    "configurations": []
}
"#;
    write_if_not_exists(
        &vscode_dir.join("launch.json"),
        launch_json,
        ".vscode/launch.json",
    )?;

    // 10. Create .vscode/extensions.json
    let extensions_json = r#"{
    "recommendations": [
        "usernamehw.errorlens",
        "eamodio.gitlens",
        "esbenp.prettier-vscode"
    ]
}
"#;
    write_if_not_exists(
        &vscode_dir.join("extensions.json"),
        extensions_json,
        ".vscode/extensions.json",
    )?;

    // 11. Update .gitignore
    let gitignore_path = project_root.join(".gitignore");
    let gitignore_entries = vec![".semantiq.db", ".semantiq.db-wal", ".semantiq.db-shm"];

    if gitignore_path.exists() {
        let content = fs::read_to_string(&gitignore_path)?;
        let mut entries_to_add = Vec::new();

        for entry in &gitignore_entries {
            if !content.contains(entry) {
                entries_to_add.push(*entry);
            }
        }

        if !entries_to_add.is_empty() {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().append(true).open(&gitignore_path)?;
            writeln!(file, "\n# Semantiq")?;
            for entry in &entries_to_add {
                writeln!(file, "{}", entry)?;
            }
            println!("Added Semantiq entries to .gitignore");
        } else {
            println!("Skipped .gitignore (entries already present)");
        }
    } else {
        let content = format!("# Semantiq\n{}\n", gitignore_entries.join("\n"));
        fs::write(&gitignore_path, content)?;
        println!("Created .gitignore");
    }

    // 12. Point terminal-capable agents at the CLI
    upsert_managed_block(&project_root.join("AGENTS.md"), AGENTS_MD_BLOCK)?;

    println!("\n✓ Cursor/VS Code configuration initialized!");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_init_cursor_creates_files() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        init_cursor(path).await.unwrap();

        // Check .cursor directory structure
        assert!(path.join(".cursor").exists());
        assert!(path.join(".cursor/rules").exists());
        assert!(path.join(".cursor/rules/project.mdc").exists());
        assert!(path.join(".cursor/rules/semantiq.mdc").exists());
        assert!(path.join(".cursor/mcp.json").exists());
        let agents_md = fs::read_to_string(path.join("AGENTS.md")).unwrap();
        assert!(agents_md.contains("semantiq refs <symbol>"));

        // Check .cursorignore
        assert!(path.join(".cursorignore").exists());

        // Check .vscode directory
        assert!(path.join(".vscode").exists());
        assert!(path.join(".vscode/settings.json").exists());
        assert!(path.join(".vscode/tasks.json").exists());
        assert!(path.join(".vscode/launch.json").exists());
        assert!(path.join(".vscode/extensions.json").exists());
    }

    #[tokio::test]
    async fn test_init_cursor_skips_existing_files() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        // Create .cursorignore with custom content
        let custom_content = "# My custom ignore\ncustom/";
        fs::write(path.join(".cursorignore"), custom_content).unwrap();

        init_cursor(path).await.unwrap();

        // Check that custom content was preserved
        let content = fs::read_to_string(path.join(".cursorignore")).unwrap();
        assert_eq!(content, custom_content);
    }

    #[tokio::test]
    async fn test_init_cursor_mcp_json_content() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        init_cursor(path).await.unwrap();

        let content = fs::read_to_string(path.join(".cursor/mcp.json")).unwrap();
        assert!(content.contains("semantiq"));
        assert!(content.contains("serve"));
    }

    #[tokio::test]
    async fn test_init_cursor_creates_gitignore() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        init_cursor(path).await.unwrap();

        let content = fs::read_to_string(path.join(".gitignore")).unwrap();
        assert!(content.contains(".semantiq.db"));
        assert!(content.contains(".semantiq.db-wal"));
        assert!(content.contains(".semantiq.db-shm"));
    }

    #[tokio::test]
    async fn test_init_cursor_updates_existing_gitignore() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        // Create existing .gitignore
        let existing = "# My project\nnode_modules/\n*.log\n";
        fs::write(path.join(".gitignore"), existing).unwrap();

        init_cursor(path).await.unwrap();

        let content = fs::read_to_string(path.join(".gitignore")).unwrap();
        // Original content preserved
        assert!(content.contains("node_modules/"));
        assert!(content.contains("*.log"));
        // New entries added
        assert!(content.contains(".semantiq.db"));
        assert!(content.contains("# Semantiq"));
    }

    #[tokio::test]
    async fn test_init_cursor_skips_existing_gitignore_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        // Create .gitignore with Semantiq entries already present
        let existing = "# Semantiq\n.semantiq.db\n.semantiq.db-wal\n.semantiq.db-shm\n";
        fs::write(path.join(".gitignore"), existing).unwrap();

        init_cursor(path).await.unwrap();

        let content = fs::read_to_string(path.join(".gitignore")).unwrap();
        // Should not duplicate the "# Semantiq" header
        assert_eq!(content.matches("# Semantiq").count(), 1);
        // Content should be unchanged
        assert_eq!(content, existing);
    }
}
