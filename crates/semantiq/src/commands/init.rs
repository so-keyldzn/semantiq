//! Initialize Semantiq for a project: installs the Claude Code skill (CLI
//! usage), registers the MCP server, points CLAUDE.md at both, and indexes.
//!
//! The skill only enters the context when it is relevant, whereas MCP tool
//! definitions are loaded in every session; both are installed by default
//! until benchmarks settle which one agents use best. `--no-mcp` and
//! `--no-skill` keep just one.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

use super::common::resolve_project_root;
use super::index::index;

/// Skill files bundled in the binary, installed under `.claude/skills/semantiq/`.
const SKILL_FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../../../skills/semantiq/SKILL.md"),
    ),
    (
        "REFERENCE.md",
        include_str!("../../../../skills/semantiq/REFERENCE.md"),
    ),
];

/// Skill location relative to a project root or the home directory.
const SKILL_DIR: &str = ".claude/skills/semantiq";

const BLOCK_START: &str = "<!-- semantiq:start -->";
const BLOCK_END: &str = "<!-- semantiq:end -->";

/// Managed block kept in the project's CLAUDE.md, matching what was
/// installed. Short on purpose: CLAUDE.md is loaded in every session, the
/// details live in the skill.
fn claude_md_block(options: InitOptions) -> String {
    let mut block = String::from(
        "## Semantiq\n\n\
         This project is indexed by Semantiq (`.semantiq.db`). To find code by concept,\n\
         trace a symbol's usages, check what a change breaks, or see a file's\n\
         dependencies, run the `semantiq` CLI through Bash",
    );
    if !options.no_skill {
        block.push_str(
            " — the `semantiq` skill\n(`.claude/skills/semantiq/SKILL.md`) explains when and how",
        );
    }
    block.push_str(
        ":\n`semantiq search \"<what the code does>\"`, `semantiq refs <symbol>`,\n\
         `semantiq impact <symbol>`, `semantiq explain <symbol>`, `semantiq deps <file>`\n\
         (add `--json` to chain).",
    );
    if !options.no_mcp {
        block.push_str(
            " The same capabilities are available as the `semantiq_*` MCP\n\
             tools (`semantiq_search`, `semantiq_find_refs`, `semantiq_impact`,\n\
             `semantiq_explain`, `semantiq_deps`).",
        );
    }
    block.push_str(" Keep grep for exact strings.");
    block
}

/// CLAUDE.md written by `semantiq init` before the skill existed. Replaced
/// wholesale when found unmodified, since it tells agents to use MCP tools.
const LEGACY_CLAUDE_MD: &str = include_str!("init_legacy_claude.md");

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct InitOptions {
    /// Do not register the MCP server in `.mcp.json`.
    pub no_mcp: bool,
    /// Do not install the skill in `.claude/skills/semantiq/`.
    pub no_skill: bool,
    /// Overwrite skill files that differ from the bundled version.
    pub force: bool,
    /// Install the skill in `~/.claude/skills/` only.
    pub global: bool,
    /// Skip the initial indexing.
    pub no_index: bool,
}

pub(crate) async fn init(path: &Path, options: InitOptions) -> Result<()> {
    if options.global {
        let home = std::env::home_dir().context("Could not determine the home directory")?;
        let dir = home.join(SKILL_DIR);
        install_skill(&dir, options.force)?;
        println!("\n✓ Semantiq skill installed for all projects in {:?}", dir);
        println!("  Index each project with `semantiq index` before using it.");
        return Ok(());
    }

    let project_root = resolve_project_root(path)?;
    println!("Initializing Semantiq for {:?}", project_root);

    setup_project(&project_root, options)?;

    if options.no_index {
        println!("\nSkipped indexing: run `semantiq index` before the first query.");
    } else {
        println!("\nIndexing project...");
        index(&project_root, None, false).await?;
    }

    println!("\n✓ Semantiq initialized successfully!");
    println!("\nNext steps:");
    match (options.no_skill, options.no_mcp) {
        (false, false) => {
            println!("  1. Restart Claude Code to load the `semantiq` skill and MCP tools")
        }
        (false, true) => println!("  1. Restart Claude Code to load the `semantiq` skill"),
        (true, false) => println!("  1. Restart Claude Code to load the semantiq MCP tools"),
        (true, true) => println!("  1. Point your agent at the `semantiq` CLI"),
    }
    println!("  2. Try it: semantiq search \"<what the code does>\"");

    Ok(())
}

/// Write every project file `init` manages (everything except the index).
fn setup_project(project_root: &Path, options: InitOptions) -> Result<()> {
    if !options.no_skill {
        install_skill(&project_root.join(SKILL_DIR), options.force)?;
    }
    upsert_managed_block(&project_root.join("CLAUDE.md"), &claude_md_block(options))?;

    let mcp_json = project_root.join(".mcp.json");
    if !options.no_mcp {
        write_mcp_json(&mcp_json)?;
    } else if mcp_json_has_semantiq(&mcp_json) {
        println!("Kept the existing `semantiq` entry in .mcp.json (remove it to disable MCP)");
    }

    update_gitignore(&project_root.join(".gitignore"))
}

/// Install the bundled skill files into `dir`. A file that differs from the
/// bundled version (edited by the user, or from an older release) is only
/// replaced with `force`.
fn install_skill(dir: &Path, force: bool) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    for (name, content) in SKILL_FILES {
        let path = dir.join(name);
        let label = display_path(&path);
        match fs::read_to_string(&path) {
            Ok(existing) if existing == *content => println!("{} is up to date", label),
            Ok(_) if !force => println!(
                "Kept {} (differs from the bundled version; use --force to replace it)",
                label
            ),
            Ok(_) => {
                fs::write(&path, content)?;
                println!("Replaced {}", label);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::write(&path, content)?;
                println!("Created {}", label);
            }
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to read {}", path.display()));
            }
        }
    }
    Ok(())
}

/// `path` relative to the current directory when possible, for messages.
fn display_path(path: &Path) -> String {
    let relative = std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.canonicalize().ok())
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(PathBuf::from));
    relative
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

/// Create an agent instructions file (CLAUDE.md, AGENTS.md) holding the
/// Semantiq block `body`, or refresh the block in place, or append it, leaving
/// the rest of the file untouched.
pub(super) fn upsert_managed_block(path: &Path, body: &str) -> Result<()> {
    let block = format!("{}\n{}\n{}", BLOCK_START, body, BLOCK_END);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let existing = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::write(path, format!("# Project Intelligence\n\n{}\n", block))?;
            println!("Created {}", name);
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    };

    if existing == LEGACY_CLAUDE_MD {
        fs::write(path, format!("# Project Intelligence\n\n{}\n", block))?;
        println!(
            "Updated {} (replaced the previous Semantiq instructions)",
            name
        );
        return Ok(());
    }

    let updated = match (existing.find(BLOCK_START), existing.find(BLOCK_END)) {
        (Some(start), Some(end)) if start < end => format!(
            "{}{}{}",
            &existing[..start],
            block,
            &existing[end + BLOCK_END.len()..]
        ),
        _ => format!("{}\n\n{}\n", existing.trim_end(), block),
    };

    if updated == existing {
        println!("{} is up to date", name);
    } else {
        fs::write(path, updated)?;
        println!("Updated {} (Semantiq section)", name);
    }
    Ok(())
}

fn update_gitignore(path: &Path) -> Result<()> {
    // Covers the WAL/SHM sidecar files too.
    let entry = ".semantiq.db*";

    match fs::read_to_string(path) {
        Ok(content) if content.contains(".semantiq.db") => {}
        Ok(_) => {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().append(true).open(path)?;
            writeln!(file, "\n# Semantiq\n{}", entry)?;
            println!("Added {} to .gitignore", entry);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::write(path, format!("# Semantiq\n{}\n", entry))?;
            println!("Created .gitignore");
        }
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
    Ok(())
}

fn mcp_json_has_semantiq(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .is_some_and(|root| root["mcpServers"]["semantiq"].is_object())
}

/// Writes or merges the `semantiq` MCP server entry into `.mcp.json`.
///
/// If the file exists, parse it and merge the entry under `mcpServers.semantiq`,
/// preserving any other servers already configured. If parsing fails, abort
/// rather than clobber user data.
fn write_mcp_json(path: &Path) -> Result<()> {
    let semantiq_entry = json!({
        "command": "semantiq",
        "args": ["serve"],
    });

    let file_existed = path.exists();
    let mut root: Value = if file_existed {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse existing {} as JSON", path.display()))?
    } else {
        json!({})
    };

    let obj = root
        .as_object_mut()
        .context(".mcp.json must be a JSON object at the top level")?;

    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context(".mcp.json's `mcpServers` field must be an object")?;

    if servers.get("semantiq") == Some(&semantiq_entry) {
        println!(".mcp.json is up to date");
        return Ok(());
    }
    let entry_replaced = servers
        .insert("semantiq".to_string(), semantiq_entry)
        .is_some();

    let pretty = serde_json::to_string_pretty(&root)?;
    fs::write(path, format!("{}\n", pretty))
        .with_context(|| format!("Failed to write {}", path.display()))?;

    let msg = match (file_existed, entry_replaced) {
        (false, _) => "Created .mcp.json",
        (true, false) => "Updated .mcp.json (added `semantiq` entry)",
        (true, true) => "Updated .mcp.json (replaced existing `semantiq` entry)",
    };
    println!("{}", msg);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn skill_path(root: &Path) -> PathBuf {
        root.join(SKILL_DIR).join("SKILL.md")
    }

    fn read(root: &Path, name: &str) -> String {
        fs::read_to_string(root.join(name)).unwrap()
    }

    #[test]
    fn test_setup_installs_skill_and_mcp_by_default() {
        let dir = tempdir().unwrap();
        setup_project(dir.path(), InitOptions::default()).unwrap();

        let skill = fs::read_to_string(skill_path(dir.path())).unwrap();
        assert!(skill.starts_with("---\nname: semantiq\n"));
        assert!(dir.path().join(SKILL_DIR).join("REFERENCE.md").is_file());

        let root: Value = serde_json::from_str(&read(dir.path(), ".mcp.json")).unwrap();
        assert_eq!(root["mcpServers"]["semantiq"]["command"], "semantiq");

        let claude_md = read(dir.path(), "CLAUDE.md");
        assert!(claude_md.contains(BLOCK_START) && claude_md.contains(BLOCK_END));
        assert!(claude_md.contains("semantiq refs <symbol>"));
        assert!(claude_md.contains(".claude/skills/semantiq/SKILL.md"));
        assert!(claude_md.contains("semantiq_find_refs"));

        assert!(read(dir.path(), ".gitignore").contains(".semantiq.db*"));
    }

    #[test]
    fn test_setup_no_mcp_installs_skill_only() {
        let dir = tempdir().unwrap();
        let options = InitOptions {
            no_mcp: true,
            ..Default::default()
        };
        setup_project(dir.path(), options).unwrap();

        assert!(skill_path(dir.path()).is_file());
        assert!(!dir.path().join(".mcp.json").exists());
        let claude_md = read(dir.path(), "CLAUDE.md");
        assert!(claude_md.contains("SKILL.md"));
        assert!(!claude_md.contains("semantiq_"));
    }

    #[test]
    fn test_setup_no_skill_registers_mcp_only() {
        let dir = tempdir().unwrap();
        let options = InitOptions {
            no_skill: true,
            ..Default::default()
        };
        setup_project(dir.path(), options).unwrap();

        assert!(!dir.path().join(".claude").exists());
        assert!(dir.path().join(".mcp.json").is_file());
        let claude_md = read(dir.path(), "CLAUDE.md");
        assert!(!claude_md.contains("SKILL.md"));
        assert!(claude_md.contains("semantiq_search"));
    }

    #[test]
    fn test_setup_is_idempotent() {
        let dir = tempdir().unwrap();
        setup_project(dir.path(), InitOptions::default()).unwrap();
        let snapshot = |name: &str| fs::read_to_string(dir.path().join(name)).unwrap();
        let (claude_md, gitignore) = (snapshot("CLAUDE.md"), snapshot(".gitignore"));
        let mcp_json = dir.path().join(".mcp.json");
        let mcp_mtime = fs::metadata(&mcp_json).unwrap().modified().unwrap();

        setup_project(dir.path(), InitOptions::default()).unwrap();
        assert_eq!(
            fs::metadata(&mcp_json).unwrap().modified().unwrap(),
            mcp_mtime
        );
        assert_eq!(snapshot("CLAUDE.md"), claude_md);
        assert_eq!(snapshot(".gitignore"), gitignore);
        assert_eq!(claude_md.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn test_modified_skill_kept_unless_force() {
        let dir = tempdir().unwrap();
        setup_project(dir.path(), InitOptions::default()).unwrap();
        let path = skill_path(dir.path());
        fs::write(&path, "my own notes").unwrap();

        setup_project(dir.path(), InitOptions::default()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "my own notes");

        let force = InitOptions {
            force: true,
            ..Default::default()
        };
        setup_project(dir.path(), force).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), SKILL_FILES[0].1);
    }

    #[test]
    fn test_claude_md_block_appended_and_refreshed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, "# My project\n\nKeep tabs.\n").unwrap();

        let block = claude_md_block(InitOptions::default());
        upsert_managed_block(&path, &block).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# My project\n\nKeep tabs.\n\n<!-- semantiq:start -->"));

        // An outdated block is replaced in place, surrounding text preserved.
        let outdated = format!(
            "# My project\n\n{}\nold text\n{}\n\n## After\n",
            BLOCK_START, BLOCK_END
        );
        fs::write(&path, &outdated).unwrap();
        upsert_managed_block(&path, &block).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("old text"));
        assert!(content.contains(&block));
        assert!(content.ends_with(&format!("{}\n\n## After\n", BLOCK_END)));
    }

    #[test]
    fn test_legacy_claude_md_replaced() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, LEGACY_CLAUDE_MD).unwrap();

        upsert_managed_block(&path, &claude_md_block(InitOptions::default())).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("Available MCP Tools"));
        assert!(content.contains(BLOCK_START));
    }

    #[test]
    fn test_mcp_entry_kept_with_no_mcp() {
        let dir = tempdir().unwrap();
        let mcp_json = dir.path().join(".mcp.json");
        write_mcp_json(&mcp_json).unwrap();
        let before = fs::read_to_string(&mcp_json).unwrap();

        let options = InitOptions {
            no_mcp: true,
            ..Default::default()
        };
        setup_project(dir.path(), options).unwrap();
        assert_eq!(fs::read_to_string(&mcp_json).unwrap(), before);
        assert!(mcp_json_has_semantiq(&mcp_json));
    }
}
