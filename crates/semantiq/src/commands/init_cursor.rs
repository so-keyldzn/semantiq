//! Initialize Cursor/VS Code configuration for a project

use anyhow::Result;
use std::fs;
use std::path::Path;

use super::common::resolve_project_root;

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

pub async fn init_cursor(path: &Path) -> Result<()> {
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

- `semantiq_search` - Search code semantically
- `semantiq_find_refs` - Find symbol references
- `semantiq_deps` - Analyze dependencies
- `semantiq_explain` - Explain symbols

## Usage Guidelines

**Always prefer Semantiq tools over grep/find for code exploration.**

| Instead of... | Use... |
|---------------|--------|
| grep, rg | `semantiq_search` |
| find, ls | `semantiq_search` |
| Manual symbol tracing | `semantiq_find_refs` |
| Reading imports manually | `semantiq_deps` |

## Best Practices

1. Use `semantiq_search` first to find relevant code before making changes
2. Use `semantiq_find_refs` to understand impact before refactoring
3. Use `semantiq_deps` to understand module relationships
4. Use `semantiq_explain` for unfamiliar symbols
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
