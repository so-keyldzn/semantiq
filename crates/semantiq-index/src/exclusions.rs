//! File exclusion logic for indexing
//!
//! This module provides common exclusion patterns for files and directories
//! that should not be indexed (VCS state, dependencies, caches, build output,
//! Semantiq's own files, large files) and the one [`WalkBuilder`]
//! configuration every walk of a project uses, so that the initial index, the
//! query-side staleness check, the text search and the watcher agree on what
//! belongs to the project.

use ignore::{DirEntry, WalkBuilder};
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

/// Directory (and file) names excluded from indexing wherever they appear.
///
/// Hidden directories are NOT excluded as such: `.github`, `.storybook`,
/// `.vscode`, `.config`, `.husky`, `.changeset`, `.devcontainer`, `.claude`...
/// hold real source and configuration and follow the project's `.gitignore`
/// like any other directory. What is excluded by name is what never holds
/// project source: VCS state, dependency trees, build output, tool caches and
/// IDE state. Semantiq's own files (`.semantiq.db*`) are excluded by prefix,
/// see [`should_exclude_entry`].
pub const EXCLUDED_DIRS: &[&str] = &[
    // Version control
    ".git",
    ".hg",
    ".svn",
    ".jj",
    // Dependencies
    "node_modules",
    "bower_components",
    "jspm_packages",
    "vendor",
    "venv",
    ".venv",
    ".yarn",
    ".pnp.cjs",
    ".pnpm-store",
    ".bundle",
    ".npm",
    ".m2",
    ".gem",
    ".gradle",
    ".stack-work",
    ".dart_tool",
    ".terraform",
    // Build output
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".nuxt",
    ".output",
    ".svelte-kit",
    ".angular",
    ".astro",
    ".docusaurus",
    ".expo",
    ".open-next",
    ".serverless",
    ".vercel",
    ".netlify",
    ".firebase",
    ".wrangler",
    ".eggs",
    // Caches
    "__pycache__",
    ".cache",
    ".parcel-cache",
    ".turbo",
    ".nx",
    ".nyc_output",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".sass-cache",
    ".ccls-cache",
    ".clangd",
    ".metals",
    ".bloop",
    ".bsp",
    ".direnv",
    ".devenv",
    ".ipynb_checkpoints",
    // IDE state
    ".idea",
    ".vs",
    ".vscode-test",
    ".history",
];

/// Prefix of Semantiq's own files next to the project (`.semantiq.db`,
/// `.semantiq.db-wal`, `.semantiq.db-shm`), never indexed and never watched.
const SEMANTIQ_PREFIX: &str = ".semantiq";

/// Check if a path should be excluded from indexing
///
/// Returns true if any component of the path is an excluded name (see
/// [`should_exclude_entry`]). Evaluated on paths relative to the project
/// root: a project living under `~/.cache/x` is not excluded as a whole.
pub fn should_exclude_path(path: &Path) -> bool {
    path.components().any(|component| match component {
        std::path::Component::Normal(name) => should_exclude_entry(&name.to_string_lossy()),
        _ => false,
    })
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
/// Returns true for the names in [`EXCLUDED_DIRS`] and for Semantiq's own
/// files (`.semantiq*`). Other hidden names are kept: `.github/workflows`,
/// `.storybook/main.ts` or `.eslintrc.json` are project source and follow
/// `.gitignore` like everything else.
pub fn should_exclude_entry(name: &str) -> bool {
    name.starts_with(SEMANTIQ_PREFIX) || EXCLUDED_DIRS.contains(&name)
}

/// Whether a walked entry is excluded by name ([`should_exclude_entry`]).
/// The root of the walk (depth 0) is never excluded: the project itself may
/// be named `build` or live under `.cache`.
pub fn should_exclude_dir_entry(entry: &DirEntry) -> bool {
    entry.depth() > 0 && should_exclude_entry(&entry.file_name().to_string_lossy())
}

/// The walk every indexing path uses for `root`: hidden entries included,
/// `.gitignore`, the global git excludes and `.git/info/exclude` honoured,
/// excluded names ([`should_exclude_dir_entry`]) pruned. Symlinks are not
/// followed. Build with `.build()`, or chain further settings first.
pub fn project_walker(root: &Path) -> WalkBuilder {
    project_walker_with(root, |_| true)
}

/// [`project_walker`] with an additional predicate: an entry is walked only
/// when it is not excluded and `keep` accepts it (a directory `keep` rejects
/// is not descended into).
pub fn project_walker_with<F>(root: &Path, keep: F) -> WalkBuilder
where
    F: Fn(&DirEntry) -> bool + Send + Sync + 'static,
{
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(move |entry| !should_exclude_dir_entry(entry) && keep(entry));
    builder
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
    fn test_should_exclude_vcs_cache_and_semantiq_dirs() {
        assert!(should_exclude_path(Path::new(".git/config")));
        assert!(should_exclude_path(Path::new(".jj/repo/store")));
        assert!(should_exclude_path(Path::new("src/.cache/file.rs")));
        assert!(should_exclude_path(Path::new(".idea/workspace.xml")));
        assert!(should_exclude_path(Path::new(".venv/lib/python3/site.py")));
        assert!(should_exclude_path(Path::new(".semantiq.db")));
        assert!(should_exclude_path(Path::new(".semantiq.db-wal")));
        assert!(should_exclude_path(Path::new(".semantiq/cache.json")));
    }

    #[test]
    fn test_hidden_tooling_dirs_are_indexed() {
        for path in [
            ".storybook/main.ts",
            ".github/workflows/ci.yml",
            ".github/scripts/release.sh",
            ".vscode/tasks.json",
            ".config/vite.config.ts",
            ".husky/pre-commit.sh",
            ".changeset/config.json",
            ".devcontainer/devcontainer.json",
            ".claude/settings.json",
            ".eslintrc.json",
        ] {
            assert!(!should_exclude_path(Path::new(path)), "{path}");
        }
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
        assert!(should_exclude_entry(".semantiq.db"));
        assert!(should_exclude_entry(".semantiq.db-shm"));
        assert!(should_exclude_entry(".mypy_cache"));
        assert!(!should_exclude_entry("src"));
        assert!(!should_exclude_entry("lib"));
        assert!(!should_exclude_entry(".github"));
        assert!(!should_exclude_entry(".storybook"));
        assert!(!should_exclude_entry(".env"));
    }

    /// The shared walk: hidden tooling directories are walked, excluded names
    /// and `.gitignore`d paths are not, and the root is never excluded by its
    /// own name.
    #[test]
    fn test_project_walker_rules() {
        let tmp = tempfile::TempDir::new().unwrap();
        // The project itself is named like an excluded directory.
        let root = tmp.path().join("build");
        for (rel, content) in [
            ("src/main.rs", "fn main() {}"),
            (".github/workflows/ci.yml", "on: push"),
            (".storybook/main.ts", "export const framework = 'x';"),
            (".vscode/settings.json", "{}"),
            ("node_modules/x/index.js", "x"),
            (".idea/workspace.xml", "<x/>"),
            (".semantiq.db", "sqlite"),
            ("generated/schema.ts", "export type S = 1;"),
            (".gitignore", "generated/\n"),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        // `.gitignore` is only honoured inside a git repository.
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let mut walked: Vec<String> = project_walker(&root)
            .build()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        walked.sort();
        assert_eq!(
            walked,
            [
                ".github/workflows/ci.yml",
                ".gitignore",
                ".storybook/main.ts",
                ".vscode/settings.json",
                "src/main.rs",
            ]
        );
    }
}
