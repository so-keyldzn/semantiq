//! `semantiq index --embeddings-only` through the binary (#122): after
//! `--no-embeddings` every chunk is pending, `--embeddings-only` embeds them
//! (stub model: zero vectors, still stored) and nothing else changes.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn semantiq(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_semantiq"))
        .args(args)
        .current_dir(dir)
        .env("SEMANTIQ_EMBEDDINGS", "stub")
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

/// The `Chunks embedded: <embedded>/<total>` line of `semantiq stats`.
fn embedded_counts(dir: &Path) -> (usize, usize) {
    let output = semantiq(dir, &["stats"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with("Chunks embedded:"))
        .unwrap_or_else(|| panic!("no `Chunks embedded` line in:\n{out}"));
    let counts = line.split(':').nth(1).unwrap().trim();
    let counts = counts.split_whitespace().next().unwrap();
    let (embedded, total) = counts.split_once('/').unwrap();
    (embedded.parse().unwrap(), total.parse().unwrap())
}

fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn compute(a: u32) -> u32 {\n    a + 1\n}\n\npub fn twice(x: u32) -> u32 {\n    compute(x) * 2\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    println!(\"{}\", twice(1));\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn embeddings_only_embeds_the_pending_chunks() {
    let project = project();
    let dir = project.path();

    let output = semantiq(dir, &["index", ".", "--no-embeddings"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let (embedded, total) = embedded_counts(dir);
    assert!(total > 0);
    assert_eq!(embedded, 0, "--no-embeddings leaves every chunk pending");

    let output = semantiq(dir, &["index", ".", "--embeddings-only"]);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    let (embedded, total_after) = embedded_counts(dir);
    assert_eq!(
        total_after, total,
        "--embeddings-only does not reindex the structure"
    );
    assert_eq!(embedded, total, "every pending chunk is embedded");

    // Idempotent: nothing left to embed, still a success.
    let output = semantiq(dir, &["index", ".", "--embeddings-only"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(embedded_counts(dir), (total, total));
}

#[test]
fn embeddings_only_without_an_index_is_an_error() {
    let dir = TempDir::new().unwrap();
    let output = semantiq(dir.path(), &["index", ".", "--embeddings-only"]);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn embeddings_only_conflicts_with_force() {
    let dir = TempDir::new().unwrap();
    let output = semantiq(dir.path(), &["index", ".", "--embeddings-only", "--force"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}
