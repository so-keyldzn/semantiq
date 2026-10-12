//! `semantiq init-cursor` through the binary (#122): the Cursor and VS Code
//! files, the AGENTS.md block, the `.gitignore` entries, and idempotence.

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn semantiq(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_semantiq"))
        .args(args)
        .current_dir(dir)
        .env("SEMANTIQ_UPDATE_CHECK", "false")
        .output()
        .expect("failed to run semantiq")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const CREATED_FILES: &[&str] = &[
    ".cursor/rules/project.mdc",
    ".cursor/rules/semantiq.mdc",
    ".cursor/mcp.json",
    ".cursorignore",
    ".vscode/settings.json",
    ".vscode/tasks.json",
    ".vscode/launch.json",
    ".vscode/extensions.json",
    ".gitignore",
    "AGENTS.md",
];

#[test]
fn init_cursor_creates_the_editor_config_and_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    let output = semantiq(root, &["init-cursor", "."]);
    assert!(output.status.success(), "{}", stderr(&output));
    for file in CREATED_FILES {
        assert!(root.join(file).is_file(), "{file} missing");
    }
    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".cursor/mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["semantiq"]["args"][0], "serve");
    for file in [
        ".vscode/settings.json",
        ".vscode/tasks.json",
        ".vscode/launch.json",
        ".vscode/extensions.json",
    ] {
        let content = fs::read_to_string(root.join(file)).unwrap();
        serde_json::from_str::<Value>(&content)
            .unwrap_or_else(|e| panic!("{file} is not JSON ({e}): {content}"));
    }
    let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(agents.contains("<!-- semantiq:start -->"), "{agents}");
    assert!(agents.contains("## Semantiq"), "{agents}");
    let gitignore = fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(gitignore.contains(".semantiq.db"), "{gitignore}");

    // Second run: every file is kept as is.
    let snapshot: Vec<(String, String)> = CREATED_FILES
        .iter()
        .map(|f| (f.to_string(), fs::read_to_string(root.join(f)).unwrap()))
        .collect();
    let output = semantiq(root, &["init-cursor", "."]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("Skipped .cursor/mcp.json (already exists)"),
        "{out}"
    );
    for (file, content) in snapshot {
        assert_eq!(
            fs::read_to_string(root.join(&file)).unwrap(),
            content,
            "{file} changed"
        );
    }
}

#[test]
fn init_cursor_keeps_user_files_and_appends_to_existing_ones() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join(".cursorignore"), "my-secrets/\n").unwrap();
    fs::write(root.join(".gitignore"), "target/\n").unwrap();
    fs::write(root.join("AGENTS.md"), "# My project\n\nKeep this.\n").unwrap();

    let output = semantiq(root, &["init-cursor"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(root.join(".cursorignore")).unwrap(),
        "my-secrets/\n"
    );
    let gitignore = fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(gitignore.starts_with("target/\n"), "{gitignore}");
    assert!(gitignore.contains(".semantiq.db"), "{gitignore}");
    let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(
        agents.starts_with("# My project\n\nKeep this.\n"),
        "{agents}"
    );
    assert!(agents.contains("<!-- semantiq:start -->"), "{agents}");
}

#[test]
fn init_cursor_explicit_path_is_created_and_used() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("nested/project");
    fs::create_dir_all(&target).unwrap();

    let output = semantiq(dir.path(), &["init-cursor", "nested/project"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(target.join(".cursor/mcp.json").is_file());
    assert!(!dir.path().join(".cursor").exists(), "the cwd is untouched");
}
