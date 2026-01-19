//! Initialize Cursor/VS Code configuration for a Rust project

use anyhow::Result;
use std::fs;
use std::path::Path;

pub async fn init_cursor(path: &Path) -> Result<()> {
    let project_root = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    println!("Initializing Cursor/VS Code config for {:?}", project_root);

    // 1. Create .cursor directory structure
    let cursor_dir = project_root.join(".cursor");
    let rules_dir = cursor_dir.join("rules");
    fs::create_dir_all(&rules_dir)?;
    println!("Created .cursor/rules/");

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
    fs::write(rules_dir.join("project.mdc"), project_rules_content)?;
    println!("Created .cursor/rules/project.mdc");

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
    fs::write(rules_dir.join("semantiq.mdc"), semantiq_rules_content)?;
    println!("Created .cursor/rules/semantiq.mdc");

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
    fs::write(cursor_dir.join("mcp.json"), mcp_json_content)?;
    println!("Created .cursor/mcp.json");

    // 5. Create .cursorignore
    let cursorignore_content = r#"# Build artifacts
target/

# Database files
*.db
*.db-wal
*.db-shm
.semantiq.db*

# Model files
*.onnx
models/

# Dependencies
node_modules/

# Version control
.git/

# IDE
.idea/

# Logs
*.log
"#;
    fs::write(project_root.join(".cursorignore"), cursorignore_content)?;
    println!("Created .cursorignore");

    // 6. Create .vscode directory
    let vscode_dir = project_root.join(".vscode");
    fs::create_dir_all(&vscode_dir)?;

    // 7. Create .vscode/settings.json
    let settings_json = r#"{
    "editor.tabSize": 4,
    "editor.formatOnSave": true,
    "editor.defaultFormatter": "rust-lang.rust-analyzer",
    "[rust]": {
        "editor.defaultFormatter": "rust-lang.rust-analyzer"
    },
    "rust-analyzer.check.command": "clippy",
    "rust-analyzer.inlayHints.parameterHints.enable": true,
    "rust-analyzer.inlayHints.typeHints.enable": true,
    "rust-analyzer.inlayHints.chainingHints.enable": true,
    "rust-analyzer.inlayHints.closingBraceHints.enable": true,
    "rust-analyzer.inlayHints.lifetimeElisionHints.enable": "skip_trivial",
    "rust-analyzer.lens.enable": true,
    "rust-analyzer.lens.run.enable": true,
    "rust-analyzer.lens.debug.enable": true,
    "files.watcherExclude": {
        "**/target/**": true,
        "**/*.db": true,
        "**/*.db-wal": true,
        "**/*.db-shm": true
    },
    "files.exclude": {
        "**/target": true
    }
}
"#;
    fs::write(vscode_dir.join("settings.json"), settings_json)?;
    println!("Created .vscode/settings.json");

    // 8. Create .vscode/tasks.json
    let tasks_json = r#"{
    "version": "2.0.0",
    "tasks": [
        {
            "label": "cargo build",
            "type": "shell",
            "command": "cargo",
            "args": ["build"],
            "group": "build",
            "problemMatcher": ["$rustc"]
        },
        {
            "label": "cargo build --release",
            "type": "shell",
            "command": "cargo",
            "args": ["build", "--release"],
            "group": "build",
            "problemMatcher": ["$rustc"]
        },
        {
            "label": "cargo test",
            "type": "shell",
            "command": "cargo",
            "args": ["test"],
            "group": "test",
            "problemMatcher": ["$rustc"]
        },
        {
            "label": "cargo test -p",
            "type": "shell",
            "command": "cargo",
            "args": ["test", "-p", "${input:crateName}"],
            "group": "test",
            "problemMatcher": ["$rustc"]
        },
        {
            "label": "cargo fmt",
            "type": "shell",
            "command": "cargo",
            "args": ["fmt"],
            "problemMatcher": []
        },
        {
            "label": "cargo clippy",
            "type": "shell",
            "command": "cargo",
            "args": ["clippy"],
            "group": "build",
            "problemMatcher": ["$rustc"]
        },
        {
            "label": "cargo run -- index",
            "type": "shell",
            "command": "cargo",
            "args": ["run", "--", "index"],
            "problemMatcher": []
        },
        {
            "label": "cargo run -- serve",
            "type": "shell",
            "command": "cargo",
            "args": ["run", "--", "serve"],
            "problemMatcher": []
        },
        {
            "label": "cargo run -- stats",
            "type": "shell",
            "command": "cargo",
            "args": ["run", "--", "stats"],
            "problemMatcher": []
        }
    ],
    "inputs": [
        {
            "id": "crateName",
            "type": "promptString",
            "description": "Enter the crate name"
        }
    ]
}
"#;
    fs::write(vscode_dir.join("tasks.json"), tasks_json)?;
    println!("Created .vscode/tasks.json");

    // 9. Create .vscode/launch.json
    let launch_json = r#"{
    "version": "0.2.0",
    "configurations": [
        {
            "type": "lldb",
            "request": "launch",
            "name": "Debug: index",
            "cargo": {
                "args": ["build", "--bin=semantiq", "--package=semantiq"],
                "filter": {
                    "name": "semantiq",
                    "kind": "bin"
                }
            },
            "args": ["index"],
            "cwd": "${workspaceFolder}"
        },
        {
            "type": "lldb",
            "request": "launch",
            "name": "Debug: serve",
            "cargo": {
                "args": ["build", "--bin=semantiq", "--package=semantiq"],
                "filter": {
                    "name": "semantiq",
                    "kind": "bin"
                }
            },
            "args": ["serve"],
            "cwd": "${workspaceFolder}"
        },
        {
            "type": "lldb",
            "request": "launch",
            "name": "Debug: search",
            "cargo": {
                "args": ["build", "--bin=semantiq", "--package=semantiq"],
                "filter": {
                    "name": "semantiq",
                    "kind": "bin"
                }
            },
            "args": ["search", "${input:searchQuery}"],
            "cwd": "${workspaceFolder}"
        },
        {
            "type": "lldb",
            "request": "launch",
            "name": "Debug: unit tests",
            "cargo": {
                "args": ["test", "--no-run", "--lib", "--package=${input:testCrate}"],
                "filter": {
                    "kind": "lib"
                }
            },
            "cwd": "${workspaceFolder}"
        }
    ],
    "inputs": [
        {
            "id": "searchQuery",
            "type": "promptString",
            "description": "Enter search query"
        },
        {
            "id": "testCrate",
            "type": "promptString",
            "description": "Enter crate name to test"
        }
    ]
}
"#;
    fs::write(vscode_dir.join("launch.json"), launch_json)?;
    println!("Created .vscode/launch.json");

    // 10. Create .vscode/extensions.json
    let extensions_json = r#"{
    "recommendations": [
        "rust-lang.rust-analyzer",
        "serayuzgur.crates",
        "tamasfe.even-better-toml",
        "usernamehw.errorlens",
        "vadimcn.vscode-lldb"
    ]
}
"#;
    fs::write(vscode_dir.join("extensions.json"), extensions_json)?;
    println!("Created .vscode/extensions.json");

    println!("\n✓ Cursor/VS Code configuration created!");
    println!("\nCreated files:");
    println!("  .cursor/");
    println!("    rules/");
    println!("      project.mdc      (general project guidelines)");
    println!("      semantiq.mdc     (Semantiq MCP tools usage)");
    println!("    mcp.json           (MCP server configuration)");
    println!("  .cursorignore        (indexing exclusions)");
    println!("  .vscode/");
    println!("    settings.json      (editor settings)");
    println!("    tasks.json         (cargo tasks)");
    println!("    launch.json        (debug configurations)");
    println!("    extensions.json    (recommended extensions)");

    Ok(())
}
