//! Common utilities and constants for CLI commands

use anyhow::{Context, Result, bail};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Default database filename
pub(super) const DEFAULT_DB_NAME: &str = ".semantiq.db";

/// Resolves a path to an absolute, canonicalized project root path.
/// If the path is relative, it's joined with the current directory.
/// The result is canonicalized to resolve `..` components and symlinks.
/// A path that is not a directory is an error (see [`ensure_directory`]).
pub(super) fn resolve_project_root(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    // Canonicalize to resolve .. components and symlinks
    let root = absolute
        .canonicalize()
        .with_context(|| format!("Failed to resolve project root: {:?}", absolute))?;
    ensure_directory(&root)?;
    Ok(root)
}

/// Fails unless `path` is a directory: a project root must be one. A file
/// given as the root would otherwise get `<file>/.semantiq.db` (a cryptic
/// SQLite "unable to open" error) or, with `--database`, be indexed as the
/// only file of the project under an empty path.
pub(super) fn ensure_directory(path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    Ok(())
}

/// Returns the database path, using the provided path or defaulting to
/// `DEFAULT_DB_NAME` in the project root.
pub(super) fn resolve_db_path(database: Option<PathBuf>, project_root: &Path) -> PathBuf {
    database.unwrap_or_else(|| project_root.join(DEFAULT_DB_NAME))
}

/// Print `text` and a newline on stdout.
///
/// A reader that stops early (`semantiq map --json | head -5`) closes the
/// pipe. Rust ignores `SIGPIPE`, so `println!` would panic with "Broken pipe"
/// and exit 101; here the command ends quietly with exit code 0 instead, the
/// reader having got all it wanted.
pub(super) fn print_stdout(text: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    let written = writeln!(stdout, "{text}").and_then(|()| stdout.flush());
    match written {
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        other => other.context("Failed to write to stdout"),
    }
}

/// Append a line to the `String` report a command prints with `print_stdout`
/// at the end: `outln!(out)` for a blank line, `outln!(out, "...", args)`
/// otherwise. Writing to a `String` cannot fail.
macro_rules! outln {
    ($out:expr) => {
        $out.push('\n')
    };
    ($out:expr, $($arg:tt)*) => {{
        use std::fmt::Write as _;
        let _ = writeln!($out, $($arg)*);
    }};
}
pub(super) use outln;

/// Logs a warning when this process uses the stub embedding model, so a
/// macOS Intel / `--no-default-features` build never silently loses
/// semantic search.
pub(super) fn warn_if_semantic_search_unavailable() {
    if let Some(reason) = semantiq_embeddings::semantic_search_unavailable_reason() {
        tracing::warn!(
            "Semantic search unavailable: {}. Search falls back to symbol and text matching.",
            reason
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_resolve_project_root_absolute() {
        // An absolute path that exists on every platform (`/tmp` does not on
        // Windows, #121); canonicalize needs it to exist.
        let path = env::temp_dir();
        assert!(path.is_absolute());
        let result = resolve_project_root(&path).unwrap();
        // canonicalize resolves symlinks, so just check it's absolute
        assert!(result.is_absolute());
    }

    #[test]
    fn test_resolve_project_root_relative() {
        let path = Path::new(".");
        let result = resolve_project_root(path).unwrap();
        // canonicalize resolves symlinks, so compare canonical forms
        assert_eq!(result, env::current_dir().unwrap().canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_project_root_rejects_a_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("Cargo.toml");
        std::fs::write(&file, "[package]").unwrap();
        let err = resolve_project_root(&file).unwrap_err().to_string();
        assert!(err.contains("is not a directory"), "{err}");
        assert!(ensure_directory(dir.path()).is_ok());
        assert!(resolve_project_root(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn test_resolve_db_path_with_provided() {
        let db = Some(PathBuf::from("/custom/path.db"));
        let project = Path::new("/project");
        let result = resolve_db_path(db, project);
        assert_eq!(result, PathBuf::from("/custom/path.db"));
    }

    #[test]
    fn test_resolve_db_path_default() {
        let project = Path::new("/project");
        let result = resolve_db_path(None, project);
        assert_eq!(result, PathBuf::from("/project/.semantiq.db"));
    }
}
