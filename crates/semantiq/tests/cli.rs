//! End-to-end tests of the `semantiq` binary: query commands against a
//! temporary index (stub embeddings in the default build) and `init`.

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_semantiq"));
    cmd.args(args)
        .current_dir(dir)
        .env("SEMANTIQ_UPDATE_CHECK", "false");
    cmd
}

fn semantiq(dir: &Path, args: &[&str]) -> Output {
    command(dir, args).output().expect("failed to run semantiq")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Run a query command with `--json` and parse its stdout.
fn query_json(dir: &Path, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let output = semantiq(dir, &args);
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        stderr(&output)
    );
    serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|e| panic!("{args:?}: invalid JSON ({e}): {}", stdout(&output)))
}

/// A small Rust project, indexed. Not a hidden directory: text search skips those.
fn indexed_project() -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("semantiq-cli")
        .tempdir()
        .unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "mod util;\n\n/// Adds two numbers.\npub fn compute(a: u32, b: u32) -> u32 {\n    util::helper(a) + b\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/util.rs"),
        "pub fn helper(x: u32) -> u32 {\n    x * 2\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    // compute is mentioned in a comment\n    let v = compute(1, 2);\n    println!(\"{v}\");\n}\n",
    )
    .unwrap();

    let output = semantiq(dir.path(), &["index", "."]);
    assert!(output.status.success(), "index failed: {}", stderr(&output));
    dir
}

#[test]
fn query_commands_print_json() {
    let project = indexed_project();
    let dir = project.path();

    let refs = query_json(dir, &["refs", "compute"]);
    assert_eq!(refs["symbol"], "compute");
    assert_eq!(refs["definitions"][0]["file_path"], "src/lib.rs");
    let usages = refs["usages"].as_array().unwrap();
    // The comment mention is not a reference.
    assert_eq!(usages.len(), 1, "{usages:?}");
    assert_eq!(usages[0]["file_path"], "src/main.rs");
    assert_eq!(usages[0]["kind"], "call");

    let explain = query_json(dir, &["explain", "compute"]);
    assert_eq!(explain["found"], true);
    assert_eq!(explain["definitions"][0]["kind"], "function");

    let missing = query_json(dir, &["explain", "does_not_exist"]);
    assert_eq!(missing["found"], false);

    let impact = query_json(dir, &["impact", "helper", "--max-depth", "2"]);
    assert_eq!(impact["definitions"][0]["file_path"], "src/util.rs");
    let files: Vec<&str> = impact["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file_path"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"src/lib.rs"), "{files:?}");
    assert_eq!(impact["truncated"], false);

    let deps = query_json(dir, &["deps", "./src/lib.rs"]);
    assert_eq!(deps["file_path"], "src/lib.rs");
    assert!(deps["imports"].is_array());
    assert!(deps["imported_by"].is_array());

    let search = query_json(dir, &["search", "compute", "--limit", "5"]);
    assert_eq!(search["query"], "compute");
    let results = search["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert!(results.len() <= 5);
    assert!(
        results
            .iter()
            .any(|r| r["symbol_name"] == "compute" && r["file_path"] == "src/lib.rs"),
        "{results:?}"
    );
}

#[test]
fn query_commands_work_from_a_subdirectory() {
    let project = indexed_project();
    let refs = query_json(&project.path().join("src"), &["refs", "helper"]);
    assert_eq!(refs["definitions"][0]["file_path"], "src/util.rs");
}

#[test]
fn human_output_matches_mcp_rendering() {
    let project = indexed_project();
    let output = semantiq(project.path(), &["refs", "compute"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(
        text.starts_with("Found 2 references to 'compute'"),
        "{text}"
    );
    assert!(text.contains("📎 src/main.rs:3 [call]"), "{text}");
}

#[test]
fn queries_refresh_changed_files_unless_disabled() {
    let project = indexed_project();
    let dir = project.path();
    fs::write(
        dir.join("src/extra.rs"),
        "pub fn twice() -> u32 {\n    crate::compute(1, 1) * 2\n}\n",
    )
    .unwrap();

    let stale = query_json(dir, &["refs", "compute", "--no-refresh"]);
    assert_eq!(stale["usages"].as_array().unwrap().len(), 1);

    let output = semantiq(dir, &["refs", "compute", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("refreshed index"));
    let fresh: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let files: Vec<&str> = fresh["usages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["file_path"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"src/extra.rs"), "{files:?}");

    // A deleted file is pruned as well.
    fs::remove_file(dir.join("src/extra.rs")).unwrap();
    let pruned = query_json(dir, &["refs", "compute"]);
    assert_eq!(pruned["usages"].as_array().unwrap().len(), 1);

    // Up to date: no refresh message.
    let output = semantiq(dir, &["refs", "compute"]);
    assert!(!stderr(&output).contains("refreshed index"));
}

/// A project with a trait, an implementor and an unused private function.
fn structural_project() -> TempDir {
    let dir = indexed_project();
    fs::write(
        dir.path().join("src/shapes.rs"),
        "pub trait Shape {\n    fn area(&self) -> f64;\n}\n\n\
         pub struct Circle {\n    pub r: f64,\n}\n\n\
         impl Shape for Circle {\n    fn area(&self) -> f64 {\n        self.r * self.r * 3.14\n    }\n}\n\n\
         fn orphan() -> u32 {\n    7\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn structural_commands_print_json() {
    let project = structural_project();
    let dir = project.path();

    let callers = query_json(dir, &["calls", "helper", "--direction", "callers"]);
    assert_eq!(callers["symbol"], "helper");
    assert_eq!(callers["direction"], "callers");
    assert_eq!(callers["definitions"][0]["file_path"], "src/util.rs");
    let edges = callers["callers"].as_array().unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e["caller"] == "compute" && e["file_path"] == "src/lib.rs"),
        "{edges:?}"
    );
    assert!(callers["callees"].as_array().unwrap().is_empty());

    let callees = query_json(dir, &["calls", "compute", "--direction", "callees"]);
    let edges = callees["callees"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["callee"] == "helper"), "{edges:?}");

    let hierarchy = query_json(dir, &["hierarchy", "Shape"]);
    assert_eq!(hierarchy["symbol"], "Shape");
    let subtypes = hierarchy["subtypes"].as_array().unwrap();
    assert!(
        subtypes
            .iter()
            .any(|r| r["sub_type"] == "Circle" && r["kind"] == "implements"),
        "{subtypes:?}"
    );

    let dead = query_json(
        dir,
        &["dead-code", "--path-prefix", "src/", "--include-public"],
    );
    let names: Vec<&str> = dead["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"orphan"), "{names:?}");
    // Called from main.rs: alive.
    assert!(!names.contains(&"compute"), "{names:?}");
    assert!(dead["excluded"]["entry_points"].as_u64().unwrap() >= 1);

    // Markdown by default.
    let output = semantiq(dir, &["calls", "helper"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("compute"), "{}", stdout(&output));
}

#[test]
fn structural_commands_reject_bad_input() {
    let project = indexed_project();
    let dir = project.path();
    assert_eq!(
        semantiq(dir, &["calls", "helper", "--direction", "sideways"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(semantiq(dir, &["hierarchy"]).status.code(), Some(2));
    let output = semantiq(dir, &["dead-code", "--path-prefix", "../elsewhere"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("must not contain '..'"));
}

#[test]
fn missing_index_is_an_error_suggesting_index() {
    let dir = tempfile::Builder::new()
        .prefix("semantiq-cli")
        .tempdir()
        .unwrap();
    for args in [
        &["refs", "x"][..],
        &["search", "x"],
        &["deps", "a.rs"],
        &["calls", "x"],
        &["dead-code"],
    ] {
        let output = semantiq(dir.path(), args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(stderr(&output).contains("semantiq index"), "{args:?}");
        assert!(stdout(&output).is_empty());
    }
    // The lookup never creates a database.
    assert!(!dir.path().join(".semantiq.db").exists());
}

#[test]
fn empty_index_is_an_error() {
    let dir = tempfile::Builder::new()
        .prefix("semantiq-cli")
        .tempdir()
        .unwrap();
    // Indexing a project without source files leaves an empty database.
    assert!(semantiq(dir.path(), &["index", "."]).status.success());
    let output = semantiq(dir.path(), &["explain", "x"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("is empty"), "{}", stderr(&output));
}

#[test]
fn usage_errors_exit_with_2() {
    let dir = TempDir::new().unwrap();
    assert_eq!(semantiq(dir.path(), &["refs"]).status.code(), Some(2));
    assert_eq!(
        semantiq(dir.path(), &["impact", "x", "--max-depth", "deep"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn invalid_input_is_an_error() {
    let project = indexed_project();
    let output = semantiq(project.path(), &["deps", "../outside.rs"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("must not contain '..'"));
}

#[test]
fn init_installs_skill_and_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let skill = root.join(".claude/skills/semantiq/SKILL.md");

    let output = semantiq(root, &["init", ".", "--no-index"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(skill.is_file());
    assert!(root.join(".claude/skills/semantiq/REFERENCE.md").is_file());
    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["semantiq"]["args"][0], "serve");
    let claude_md = fs::read_to_string(root.join("CLAUDE.md")).unwrap();
    assert!(claude_md.contains("<!-- semantiq:start -->"));

    // Second run changes nothing.
    let bundled = fs::read_to_string(&skill).unwrap();
    let output = semantiq(root, &["init", ".", "--no-index"]);
    assert!(stdout(&output).contains("is up to date"));
    assert_eq!(
        fs::read_to_string(root.join("CLAUDE.md")).unwrap(),
        claude_md
    );

    // A user-edited skill survives, unless --force.
    fs::write(&skill, "custom").unwrap();
    semantiq(root, &["init", ".", "--no-index"]);
    assert_eq!(fs::read_to_string(&skill).unwrap(), "custom");
    semantiq(root, &["init", ".", "--no-index", "--force"]);
    assert_eq!(fs::read_to_string(&skill).unwrap(), bundled);
}

#[test]
fn init_no_mcp_and_no_skill() {
    let skill_only = TempDir::new().unwrap();
    let output = semantiq(skill_only.path(), &["init", ".", "--no-index", "--no-mcp"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!skill_only.path().join(".mcp.json").exists());
    assert!(
        skill_only
            .path()
            .join(".claude/skills/semantiq/SKILL.md")
            .is_file()
    );

    let mcp_only = TempDir::new().unwrap();
    let output = semantiq(mcp_only.path(), &["init", ".", "--no-index", "--no-skill"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(mcp_only.path().join(".mcp.json").is_file());
    assert!(!mcp_only.path().join(".claude").exists());
}

#[test]
fn init_indexes_by_default() {
    let project = tempfile::Builder::new()
        .prefix("semantiq-cli")
        .tempdir()
        .unwrap();
    fs::write(
        project.path().join("lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    assert!(semantiq(project.path(), &["init"]).status.success());
    let explain = query_json(project.path(), &["explain", "answer"]);
    assert_eq!(explain["found"], true);
}

#[test]
fn init_global_installs_skill_in_home() {
    let home = TempDir::new().unwrap();
    let output = command(home.path(), &["init", "--global"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        home.path()
            .join(".claude/skills/semantiq/SKILL.md")
            .is_file()
    );
    // Nothing project-level is written.
    assert!(!home.path().join("CLAUDE.md").exists());
    assert!(!home.path().join(".semantiq.db").exists());
}
