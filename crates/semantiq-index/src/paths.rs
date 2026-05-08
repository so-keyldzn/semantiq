//! Path normalization helpers for the index.
//!
//! Every path stored in the database must be **relative to the project root**
//! (see `CLAUDE.md` § "Key Internal Conventions"). Mixing absolute and relative
//! paths breaks deduplication on the search side and made it impossible to keep
//! `chunks_vec` in sync with `chunks` (the resulting orphan vectors silently
//! poison KNN). This module centralizes the conversion so the rule has exactly
//! one implementation.

use std::path::{Path, PathBuf};

/// Convert `path` to a string relative to `project_root`.
///
/// Tries a literal `strip_prefix` first (no syscalls). If that fails — usually
/// because one side has a `/private/tmp` style symlink resolved and the other
/// doesn't, or because the walker returned a canonicalized path while the
/// caller passed a logical one — falls back to canonicalizing both. This keeps
/// the hot indexing path free of `realpath` round-trips on networked
/// filesystems.
///
/// On a symlink cycle `canonicalize` returns `Err` (it's bounded internally),
/// and we fall through to the WARN branch with the original path.
///
/// Returns the relative form. If stripping fails — typically because `path` is
/// not actually inside `project_root` — a `WARN` is logged and the path is
/// returned as-is. Indexing an "outside" file is almost always a bug; the warn
/// makes it visible instead of silently inserting absolute paths.
pub fn to_relative_string(path: &Path, project_root: &Path) -> String {
    // Fast path: literal strip_prefix succeeds when both sides share the same
    // logical prefix. This is the common case during a normal walk.
    if let Ok(rel) = path.strip_prefix(project_root) {
        return rel.to_string_lossy().to_string();
    }

    // Fallback: canonicalize both and retry. Necessary on macOS where /tmp
    // resolves to /private/tmp and on systems with symlinked checkout dirs.
    let canon_root: PathBuf = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let canon_path: PathBuf = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    match canon_path.strip_prefix(&canon_root) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => {
            tracing::warn!(
                path = %path.display(),
                project_root = %project_root.display(),
                "indexed path is not inside project_root; storing path as-is. \
                 This breaks dedup and may pollute the vector index."
            );
            path.to_string_lossy().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn strips_when_inside() {
        // Build a real temp tree so canonicalize agrees on both sides
        // (avoids /tmp -> /private/tmp symlink issues on macOS).
        let root = TempDir::new().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("file.rs");
        fs::write(&file, "").unwrap();

        let s = to_relative_string(&file, root.path());
        assert_eq!(s, "sub/file.rs");
    }

    #[test]
    fn keeps_absolute_when_outside() {
        // Two unrelated temp dirs: a file under one, a project_root pointing
        // to the other. strip_prefix must fail and the path is returned as-is.
        let outside_root = TempDir::new().unwrap();
        let outside_file = outside_root.path().join("etc.conf");
        fs::write(&outside_file, "").unwrap();

        let project_root = TempDir::new().unwrap();
        let s = to_relative_string(&outside_file, project_root.path());

        // The function should preserve the absolute form (after canonicalize)
        // so downstream code at least has something it can read.
        assert!(PathBuf::from(&s).is_absolute(), "expected absolute, got: {s}");
    }
}
