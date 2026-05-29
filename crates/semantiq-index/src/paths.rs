//! Path normalization helpers for the index.
//!
//! Every path stored in the database must be **relative to the project root**
//! (see `CLAUDE.md` § "Key Internal Conventions"). Mixing absolute and relative
//! paths breaks deduplication on the search side and made it impossible to keep
//! `chunks_vec` in sync with `chunks` (the resulting orphan vectors silently
//! poison KNN). This module centralizes the conversion so the rule has exactly
//! one implementation.

use std::path::{Component, Path, PathBuf};

/// Lexically normalize a *relative* path by resolving `.` and `..` components
/// without touching the filesystem.
///
/// Returns `None` if the path escapes its base (a leading `..` that pops above
/// the root), since such a path is not actually inside `project_root` and must
/// be handled by the caller's "outside" branch.
///
/// This matters for dedup: a walker can hand us `sub/../sub/file.rs` while a
/// normal walk yields `sub/file.rs`. Without normalization these become two
/// distinct DB keys for the same physical file, which then desyncs
/// `chunks`/`chunks_vec` exactly the way absolute paths did.
fn normalize_relative(rel: &Path) -> Option<PathBuf> {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::CurDir => {} // drop "."
            Component::ParentDir => {
                // Pop the last normal component; if there is none we've escaped
                // the root.
                out.pop()?;
            }
            Component::Normal(part) => out.push(part.to_os_string()),
            // A relative path produced by strip_prefix should not contain a
            // root/prefix component; if it somehow does, bail to the safe path.
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let mut buf = PathBuf::new();
    for part in out {
        buf.push(part);
    }
    Some(buf)
}

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
    // logical prefix. This is the common case during a normal walk. We still
    // lexically normalize `.`/`..` so e.g. `sub/../sub/x.rs` and `sub/x.rs`
    // collapse to one dedup key. If normalization shows the path actually
    // escapes the root, fall through to the canonicalize/WARN branches.
    if let Ok(rel) = path.strip_prefix(project_root)
        && let Some(norm) = normalize_relative(rel)
    {
        return norm.to_string_lossy().to_string();
    }

    // Fallback: canonicalize both and retry. Necessary on macOS where /tmp
    // resolves to /private/tmp and on systems with symlinked checkout dirs.
    let canon_root: PathBuf = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let canon_path: PathBuf = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    match canon_path
        .strip_prefix(&canon_root)
        .ok()
        .and_then(normalize_relative)
    {
        Some(rel) => rel.to_string_lossy().to_string(),
        None => {
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
        assert!(
            PathBuf::from(&s).is_absolute(),
            "expected absolute, got: {s}"
        );
    }

    /// `sub/../sub/file.rs` must collapse to the same dedup key as
    /// `sub/file.rs`. Without `..` normalization these are two distinct strings
    /// for one physical file and the index would store both.
    #[test]
    fn parent_dir_components_are_normalized_for_dedup() {
        let root = TempDir::new().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("file.rs");
        fs::write(&file, "").unwrap();

        // A path with a redundant `..` round-trip, as a walker or resolver can
        // produce. strip_prefix succeeds but leaves `sub/../sub/file.rs`.
        let messy = root
            .path()
            .join("sub")
            .join("..")
            .join("sub")
            .join("file.rs");

        let clean = to_relative_string(&file, root.path());
        let normalized = to_relative_string(&messy, root.path());

        assert_eq!(normalized, "sub/file.rs");
        assert_eq!(
            normalized, clean,
            "`..` round-trip must dedup to the same relative key"
        );
    }

    /// `.` components are dropped.
    #[test]
    fn current_dir_components_are_dropped() {
        let root = TempDir::new().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("file.rs");
        fs::write(&file, "").unwrap();

        let dotted = root.path().join(".").join("sub").join(".").join("file.rs");
        assert_eq!(to_relative_string(&dotted, root.path()), "sub/file.rs");
    }

    /// A path that escapes the root via `..` is "outside" and must not be
    /// emitted as a clean in-tree relative key.
    #[test]
    fn parent_dir_that_escapes_root_is_treated_as_outside() {
        let root = TempDir::new().unwrap();
        let inner = root.path().join("inner");
        fs::create_dir_all(&inner).unwrap();
        let sibling = root.path().join("sibling.rs");
        fs::write(&sibling, "").unwrap();

        // From `inner` as project_root, `inner/../sibling.rs` escapes it.
        let escaping = inner.join("..").join("sibling.rs");
        let s = to_relative_string(&escaping, &inner);

        // Must NOT be a tidy in-tree relative like "sibling.rs"; the function
        // canonicalizes and, since it genuinely resolves outside `inner`, keeps
        // an absolute form.
        assert!(
            PathBuf::from(&s).is_absolute(),
            "escaping `..` path should not be reported as in-tree relative: {s}"
        );
    }

    /// The canonicalize fallback (literal strip_prefix fails, but both sides
    /// resolve to the same dir) still yields a normalized relative path.
    /// Exercised via a symlinked project root, which is the real-world trigger
    /// (e.g. macOS `/tmp` -> `/private/tmp`, symlinked checkout dirs).
    #[cfg(unix)]
    #[test]
    fn canonicalize_fallback_resolves_through_symlink() {
        use std::os::unix::fs::symlink;

        let base = TempDir::new().unwrap();
        let real = base.path().join("real");
        let sub = real.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("file.rs");
        fs::write(&file, "").unwrap();

        // `link` -> `real`. Passing `link` as project_root makes the literal
        // strip_prefix fail (link path is not a textual prefix of the real
        // file path), forcing the canonicalize fallback.
        let link = base.path().join("link");
        symlink(&real, &link).unwrap();

        let s = to_relative_string(&file, &link);
        assert_eq!(
            s, "sub/file.rs",
            "fallback should canonicalize both sides and strip to a relative key"
        );
        assert!(
            !PathBuf::from(&s).is_absolute(),
            "in-tree file must never come out absolute via the fallback"
        );
    }

    /// Sweep an entire real tree (including files reached via `.` and `..`):
    /// no in-tree file may ever be emitted as an absolute path.
    #[test]
    fn no_in_tree_file_is_emitted_as_absolute() {
        let root = TempDir::new().unwrap();
        let nested = root.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();

        let f1 = root.path().join("top.rs");
        let f2 = root.path().join("a").join("mid.rs");
        let f3 = nested.join("deep.rs");
        for f in [&f1, &f2, &f3] {
            fs::write(f, "").unwrap();
        }

        // Also include messy-but-in-tree spellings of the same files.
        let messy1 = root.path().join("a").join("..").join("top.rs");
        let messy2 = root.path().join(".").join("a").join("mid.rs");

        for p in [&f1, &f2, &f3, &messy1, &messy2] {
            let s = to_relative_string(p, root.path());
            assert!(
                !PathBuf::from(&s).is_absolute(),
                "in-tree path {p:?} must be stored relative, got {s}"
            );
            assert!(
                !s.contains(".."),
                "relative key must not retain `..` components: {s}"
            );
        }
    }
}
