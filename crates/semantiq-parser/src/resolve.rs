//! Resolve local import paths to actual files on disk.

use crate::language::Language;
use std::path::{Path, PathBuf};

/// Resolve a local import path to a relative file path from the project root.
///
/// Given a source file's relative path, an import path string, the language,
/// and the project root, attempts to find the actual file on disk.
///
/// Returns `Some(relative_path)` if found, `None` otherwise.
pub fn resolve_local_import(
    source_rel_path: &str,
    import_path: &str,
    language: Language,
    project_root: &Path,
) -> Option<String> {
    let source_dir = Path::new(source_rel_path).parent().unwrap_or(Path::new(""));

    let raw = match language {
        Language::Python => Some(resolve_python_import(source_dir, import_path)),
        Language::Rust => resolve_rust_import(source_rel_path, import_path),
        Language::TypeScript | Language::JavaScript => resolve_js_import(source_dir, import_path),
        _ => resolve_generic_import(source_dir, import_path),
    };

    let candidates = raw?;

    for candidate in candidates {
        // Normalize the path (resolve .. and .)
        let normalized = normalize_path(&candidate);
        let abs = project_root.join(&normalized);

        // Try exact match
        if abs.is_file() {
            return Some(normalized.to_string_lossy().to_string());
        }

        // Try with language-specific extensions
        for ext in extensions_for_language(language) {
            let with_ext = abs.with_extension(ext);
            if with_ext.is_file() {
                let rel = with_ext.strip_prefix(project_root).ok()?;
                return Some(rel.to_string_lossy().to_string());
            }
        }

        // Try index files (JS/TS)
        if matches!(language, Language::JavaScript | Language::TypeScript) {
            for index_name in &["index.ts", "index.tsx", "index.js", "index.jsx"] {
                let index_path = abs.join(index_name);
                if index_path.is_file() {
                    let rel = index_path.strip_prefix(project_root).ok()?;
                    return Some(rel.to_string_lossy().to_string());
                }
            }
        }

        // Try __init__.py (Python)
        if language == Language::Python {
            let init_path = abs.join("__init__.py");
            if init_path.is_file() {
                let rel = init_path.strip_prefix(project_root).ok()?;
                return Some(rel.to_string_lossy().to_string());
            }
        }
    }

    None
}

/// Normalize a path by resolving `.` and `..` components without touching the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
}

fn resolve_python_import(source_dir: &Path, import_path: &str) -> Vec<PathBuf> {
    if import_path.starts_with('.') {
        // Relative import: count leading dots
        let dots = import_path.chars().take_while(|c| *c == '.').count();
        let rest = &import_path[dots..];

        let mut base = source_dir.to_path_buf();
        // Each dot beyond the first goes up one directory
        for _ in 1..dots {
            base = base.join("..");
        }

        if rest.is_empty() {
            vec![base]
        } else {
            let file_path = base.join(rest.replace('.', "/"));
            vec![file_path]
        }
    } else {
        // Absolute Python import — resolve as path from project root
        let file_path = PathBuf::from(import_path.replace('.', "/"));
        vec![file_path]
    }
}

fn resolve_rust_import(source_rel_path: &str, import_path: &str) -> Option<Vec<PathBuf>> {
    // Only handle crate-local imports
    if let Some(rest) = import_path.strip_prefix("crate::") {
        // Skip glob/brace imports
        if rest.contains('{') || rest.contains('*') {
            return None;
        }
        let segments: Vec<&str> = rest.split("::").collect();
        if segments.is_empty() {
            return None;
        }
        // Find the crate's src/ directory from the source file path.
        // e.g., "crates/semantiq-index/src/store/deps.rs" -> "crates/semantiq-index/src"
        let src_dir = find_rust_src_dir(source_rel_path)?;

        let mut candidates = Vec::new();
        // Try full path: src/<all segments>.rs and src/<all segments>/mod.rs
        let file_path: PathBuf = std::iter::once(src_dir.as_str())
            .chain(segments.iter().copied())
            .collect();
        candidates.push(file_path.join("mod.rs"));
        candidates.push(file_path);
        // Also try without last segment (last may be a type/function name)
        if segments.len() > 1 {
            let module_path: PathBuf = std::iter::once(src_dir.as_str())
                .chain(segments[..segments.len() - 1].iter().copied())
                .collect();
            candidates.push(module_path.join("mod.rs"));
            candidates.push(module_path);
        }
        Some(candidates)
    } else {
        // super:: is context-dependent, skip for now
        None
    }
}

/// Find the `src/` directory prefix for a Rust source file.
/// e.g., "crates/semantiq-index/src/store/deps.rs" -> "crates/semantiq-index/src"
/// e.g., "src/main.rs" -> "src"
fn find_rust_src_dir(source_rel_path: &str) -> Option<String> {
    let path = Path::new(source_rel_path);
    for ancestor in path.ancestors() {
        if ancestor.file_name().and_then(|n| n.to_str()) == Some("src") {
            return Some(ancestor.to_string_lossy().to_string());
        }
    }
    None
}

fn resolve_js_import(source_dir: &Path, import_path: &str) -> Option<Vec<PathBuf>> {
    if import_path.starts_with('.') {
        let file_path = source_dir.join(import_path);
        Some(vec![file_path])
    } else {
        None
    }
}

fn resolve_generic_import(source_dir: &Path, import_path: &str) -> Option<Vec<PathBuf>> {
    if import_path.starts_with('.') {
        let file_path = source_dir.join(import_path);
        Some(vec![file_path])
    } else {
        None
    }
}

fn extensions_for_language(language: Language) -> &'static [&'static str] {
    match language {
        Language::TypeScript => &["ts", "tsx", "js", "jsx"],
        Language::JavaScript => &["js", "jsx", "ts", "tsx"],
        Language::Python => &["py", "pyi"],
        Language::Rust => &["rs"],
        Language::Go => &["go"],
        Language::Java => &["java"],
        Language::C => &["c", "h"],
        Language::Cpp => &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
        Language::Php => &["php"],
        Language::Ruby => &["rb"],
        Language::CSharp => &["cs"],
        Language::Kotlin => &["kt", "kts"],
        Language::Scala => &["scala", "sc"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_project(files: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for file in files {
            let path = dir.path().join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, "// placeholder").unwrap();
        }
        dir
    }

    #[test]
    fn test_resolve_js_relative_import() {
        let dir = setup_project(&["src/components/Button.tsx", "src/utils/helpers.ts"]);

        let resolved = resolve_local_import(
            "src/components/Button.tsx",
            "../utils/helpers",
            Language::TypeScript,
            dir.path(),
        );
        assert_eq!(resolved.as_deref(), Some("src/utils/helpers.ts"));
    }

    #[test]
    fn test_resolve_js_index_import() {
        let dir = setup_project(&["src/components/index.ts", "src/app.ts"]);

        let resolved = resolve_local_import(
            "src/app.ts",
            "./components",
            Language::TypeScript,
            dir.path(),
        );
        assert_eq!(resolved.as_deref(), Some("src/components/index.ts"));
    }

    #[test]
    fn test_resolve_python_relative_import() {
        let dir = setup_project(&["pkg/sub/module.py", "pkg/main.py"]);

        let resolved =
            resolve_local_import("pkg/main.py", ".sub.module", Language::Python, dir.path());
        assert_eq!(resolved.as_deref(), Some("pkg/sub/module.py"));
    }

    #[test]
    fn test_resolve_python_init_import() {
        let dir = setup_project(&["pkg/utils/__init__.py", "pkg/main.py"]);

        let resolved = resolve_local_import("pkg/main.py", ".utils", Language::Python, dir.path());
        assert_eq!(resolved.as_deref(), Some("pkg/utils/__init__.py"));
    }

    #[test]
    fn test_resolve_rust_crate_import() {
        let dir = setup_project(&["src/utils.rs"]);

        let resolved =
            resolve_local_import("src/main.rs", "crate::utils", Language::Rust, dir.path());
        assert_eq!(resolved.as_deref(), Some("src/utils.rs"));
    }

    #[test]
    fn test_resolve_nonexistent_returns_none() {
        let dir = setup_project(&["src/main.rs"]);

        let resolved = resolve_local_import(
            "src/main.rs",
            "./nonexistent",
            Language::TypeScript,
            dir.path(),
        );
        assert!(resolved.is_none());
    }

    #[test]
    fn test_resolve_external_import_returns_none() {
        let dir = setup_project(&["src/main.ts"]);

        // Non-relative imports should return None
        let resolved =
            resolve_local_import("src/main.ts", "react", Language::TypeScript, dir.path());
        assert!(resolved.is_none());
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(
            normalize_path(Path::new("src/../lib/utils")),
            PathBuf::from("lib/utils")
        );
        assert_eq!(
            normalize_path(Path::new("./src/./utils")),
            PathBuf::from("src/utils")
        );
    }

    #[test]
    fn test_resolve_blocks_path_traversal() {
        // Even if a sibling file exists outside the project root, an import that
        // escapes the root via `..` must not resolve to it. The resolver only
        // checks files inside `project_root`.
        let outer = TempDir::new().unwrap();
        // A "secret" file outside the project root
        fs::write(outer.path().join("secret.ts"), "// secret").unwrap();

        let project_root = outer.path().join("project");
        fs::create_dir_all(project_root.join("src")).unwrap();
        fs::write(project_root.join("src/app.ts"), "// app").unwrap();

        let resolved = resolve_local_import(
            "src/app.ts",
            "../../secret",
            Language::TypeScript,
            &project_root,
        );
        assert!(
            resolved.is_none(),
            "imports escaping project_root must not resolve, got {:?}",
            resolved
        );
    }

    #[test]
    fn test_python_grandparent_relative_import() {
        // `from ...module import x` (3 dots) should walk up 2 directories from
        // the source file's directory.
        // Layout: pkg/a/b/leaf.py imports `...top` -> pkg/top.py
        let dir = setup_project(&["pkg/a/b/leaf.py", "pkg/top.py"]);

        let resolved =
            resolve_local_import("pkg/a/b/leaf.py", "...top", Language::Python, dir.path());
        assert_eq!(resolved.as_deref(), Some("pkg/top.py"));
    }
}
