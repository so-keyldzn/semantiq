//! File exclusion logic for indexing
//!
//! This module provides common exclusion patterns for files and directories
//! that should not be indexed (hidden dirs, dependencies, large files, etc.)

use semantiq_parser::Language;
use std::path::Path;

/// Maximum file size in bytes (1MB)
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// Maximum size of an indexed data file (JSON, YAML, TOML): 256 KB.
///
/// Large data files (fixtures, exports, benchmark results, lockfile-like
/// dumps) carry thousands of keys and chunks but little code meaning: a single
/// 800 KB JSON file can hold most of a project's chunks and half of the
/// embedding time. Code files keep the [`MAX_FILE_SIZE`] limit.
pub const MAX_DATA_FILE_SIZE: u64 = 256 * 1024;

/// Size limit above which `path` is not indexed: [`MAX_DATA_FILE_SIZE`] for
/// data languages, [`MAX_FILE_SIZE`] otherwise.
pub fn max_indexed_size(path: &Path) -> u64 {
    match Language::from_path(path) {
        Some(Language::Json | Language::Yaml | Language::Toml) => MAX_DATA_FILE_SIZE,
        _ => MAX_FILE_SIZE,
    }
}

/// Whether a file of `size` bytes at `path` is above its indexing limit.
pub fn exceeds_indexed_size(path: &Path, size: u64) -> bool {
    size > max_indexed_size(path)
}

/// Directories to exclude from indexing
pub const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".next",
    "__pycache__",
    "venv",
    ".venv",
    "coverage",
    ".nyc_output",
    ".git",
    ".hg",
    ".svn",
    "out",
    ".output",
    ".nuxt",
    ".cache",
    ".parcel-cache",
    ".turbo",
];

/// Check if a path should be excluded from indexing
///
/// Returns true if:
/// - Any component of the path starts with '.' (hidden directory)
/// - Any component matches an excluded directory name
pub fn should_exclude_path(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            // Hidden directory (starts with .)
            if name_str.starts_with('.') {
                return true;
            }
            // Excluded directory
            if EXCLUDED_DIRS.contains(&name_str.as_ref()) {
                return true;
            }
        }
    }
    false
}

/// Check if a file should be excluded based on its size (see [`max_indexed_size`])
pub fn is_file_too_large(path: &Path) -> bool {
    if let Ok(metadata) = std::fs::metadata(path) {
        return exceeds_indexed_size(path, metadata.len());
    }
    false
}

/// Check if a path should be excluded (combines path check and file size check)
pub fn should_exclude(path: &Path) -> bool {
    should_exclude_path(path) || is_file_too_large(path)
}

/// Check if a directory entry name should be excluded (for WalkBuilder filter).
///
/// Returns true if the name matches an excluded directory or starts with '.'
/// (hidden directory). This is consistent with `should_exclude_path` behavior.
pub fn should_exclude_entry(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIRS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_files_have_a_lower_size_limit() {
        let big = 300 * 1024;
        for data in ["results.json", "config.yaml", "ci.yml", "Cargo.toml"] {
            assert!(exceeds_indexed_size(Path::new(data), big), "{data}");
            assert!(!exceeds_indexed_size(Path::new(data), MAX_DATA_FILE_SIZE));
        }
        for code in ["main.rs", "app.ts", "README.md", "noext"] {
            assert!(!exceeds_indexed_size(Path::new(code), big), "{code}");
            assert!(exceeds_indexed_size(Path::new(code), MAX_FILE_SIZE + 1));
        }
    }

    #[test]
    fn test_should_exclude_hidden_dirs() {
        assert!(should_exclude_path(Path::new(".git/config")));
        assert!(should_exclude_path(Path::new(".claude/settings.json")));
        assert!(should_exclude_path(Path::new("src/.hidden/file.rs")));
    }

    #[test]
    fn test_should_exclude_dependency_dirs() {
        assert!(should_exclude_path(Path::new(
            "node_modules/package/index.js"
        )));
        assert!(should_exclude_path(Path::new("target/debug/main")));
        assert!(should_exclude_path(Path::new(
            "vendor/github.com/pkg/file.go"
        )));
    }

    #[test]
    fn test_should_not_exclude_normal_paths() {
        assert!(!should_exclude_path(Path::new("src/main.rs")));
        assert!(!should_exclude_path(Path::new("lib/utils.ts")));
        assert!(!should_exclude_path(Path::new("packages/core/index.js")));
    }

    #[test]
    fn test_should_exclude_entry() {
        assert!(should_exclude_entry("node_modules"));
        assert!(should_exclude_entry("target"));
        assert!(should_exclude_entry(".git"));
        assert!(should_exclude_entry(".env"));
        assert!(should_exclude_entry(".secrets"));
        assert!(!should_exclude_entry("src"));
        assert!(!should_exclude_entry("lib"));
    }
}
