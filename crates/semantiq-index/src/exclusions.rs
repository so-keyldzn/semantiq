//! File exclusion logic for indexing
//!
//! This module provides common exclusion patterns for files and directories
//! that should not be indexed (hidden dirs, dependencies, large files, etc.)

use std::path::Path;

/// Maximum file size in bytes (1MB)
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;

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

/// Check if a file should be excluded based on its size
pub fn is_file_too_large(path: &Path) -> bool {
    if let Ok(metadata) = std::fs::metadata(path) {
        return metadata.len() > MAX_FILE_SIZE;
    }
    false
}

/// Check if a path should be excluded (combines path check and file size check)
pub fn should_exclude(path: &Path) -> bool {
    should_exclude_path(path) || is_file_too_large(path)
}

/// Check if a directory entry name should be excluded (for WalkBuilder filter)
pub fn should_exclude_entry(name: &str) -> bool {
    EXCLUDED_DIRS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!should_exclude_entry("src"));
        assert!(!should_exclude_entry("lib"));
    }
}
