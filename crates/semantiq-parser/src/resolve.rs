//! Resolve import paths to files of the project.

use crate::imports::ImportContext;
use crate::language::Language;
use std::path::{Component, Path, PathBuf};

/// [`resolve_import`] without project facts: relative paths, Rust module
/// paths, Python absolute imports and C / Ruby / Bash paths resolve; the
/// package-based languages (Java, Kotlin, Scala, C#, PHP, Go) need the
/// [`ImportContext`] of the project.
pub fn resolve_local_import(
    source_rel_path: &str,
    import_path: &str,
    language: Language,
    project_root: &Path,
) -> Option<String> {
    resolve_import(
        source_rel_path,
        import_path,
        language,
        project_root,
        &ImportContext::default(),
    )
}

/// Resolve an import path to the file of the project it names, as a path
/// relative to the project root with `/` separators.
///
/// `source_rel_path` is the importing file (relative to `project_root`),
/// `import_path` the import as written (`./x`, `crate::a::B`, `app.models`,
/// `"util.h"`, `com.acme.models.User`...). Resolution is syntactic plus a
/// look at the filesystem; nothing is read. The importing file itself is
/// never a result (`use super::*` in `mod tests`). Returns `None` when no
/// file of the project matches, including for every path that climbs above
/// the project root (#83).
pub fn resolve_import(
    source_rel_path: &str,
    import_path: &str,
    language: Language,
    project_root: &Path,
    context: &ImportContext,
) -> Option<String> {
    let source_dir = Path::new(source_rel_path).parent().unwrap_or(Path::new(""));

    let candidates: Vec<PathBuf> = match language {
        Language::Python => resolve_python_import(source_dir, import_path),
        Language::Rust => resolve_rust_import(source_rel_path, import_path)?,
        Language::TypeScript | Language::JavaScript => resolve_js_import(source_dir, import_path)?,
        // `#include "x.h"` is looked up next to the including file, then from
        // the project root and its `include/` directory (#142).
        Language::C | Language::Cpp => path_candidates(
            source_dir,
            import_path,
            &[Path::new(""), Path::new("include")],
        ),
        // `require_relative 'x'` is relative to the requiring file; `require
        // 'x'` goes through the load path, usually `lib/` (#142).
        Language::Ruby => {
            path_candidates(source_dir, import_path, &[Path::new(""), Path::new("lib")])
        }
        Language::Bash => path_candidates(source_dir, import_path, &[Path::new("")]),
        // Package imports: the files declaring the package, known from the
        // project walk (`ImportContext::add_source_file`, #147).
        Language::Go => return context.resolve_go_import(import_path),
        Language::Java | Language::Kotlin | Language::Scala | Language::CSharp => {
            return context.resolve_package_import(import_path, '.');
        }
        Language::Php => return context.resolve_package_import(import_path, '\\'),
        // Elixir aliases name modules, not files; data files import nothing.
        Language::Elixir | Language::Html | Language::Json | Language::Yaml | Language::Toml => {
            return None;
        }
    };

    candidates
        .iter()
        .find_map(|candidate| find_file(candidate, source_rel_path, language, project_root))
}

/// The file `candidate` (a path from the project root, possibly without its
/// extension and possibly a directory) names on disk, if any: the exact
/// path, then with each extension of `language`, then the TS counterpart of
/// an explicit JS extension, then the `index.*` / `__init__.py` of a
/// directory.
fn find_file(
    candidate: &Path,
    source_rel_path: &str,
    language: Language,
    project_root: &Path,
) -> Option<String> {
    let normalized = normalize_path(candidate)?;
    let hit = |rel: &Path| -> Option<String> {
        if !project_root.join(rel).is_file() {
            return None;
        }
        let rel = slash_path(rel);
        // The importing file is not one of its own dependencies.
        (rel != source_rel_path).then_some(rel)
    };

    if let Some(found) = hit(&normalized) {
        return Some(found);
    }

    // Language-specific extensions, appended to the full file name so
    // `./user.service` looks for `user.service.ts`, not `user.ts`.
    if let Some(file_name) = normalized.file_name() {
        let file_name = file_name.to_string_lossy();
        for ext in extensions_for_language(language) {
            if let Some(found) = hit(&normalized.with_file_name(format!("{file_name}.{ext}"))) {
                return Some(found);
            }
        }
    }

    if matches!(language, Language::JavaScript | Language::TypeScript) {
        // Explicit JS extensions may refer to TS source files.
        let replacements: &[&str] = match normalized.extension().and_then(|e| e.to_str()) {
            Some("js") | Some("mjs") => &["ts", "tsx"],
            Some("jsx") => &["tsx", "ts"],
            _ => &[],
        };
        for ext in replacements {
            if let Some(found) = hit(&normalized.with_extension(ext)) {
                return Some(found);
            }
        }
        for index_name in &["index.ts", "index.tsx", "index.js", "index.jsx"] {
            if let Some(found) = hit(&normalized.join(index_name)) {
                return Some(found);
            }
        }
    }

    if language == Language::Python
        && let Some(found) = hit(&normalized.join("__init__.py"))
    {
        return Some(found);
    }

    None
}

/// Render a relative path with `/` between its components, whatever the
/// platform separator.
///
/// Every path the index stores or emits goes through this (via
/// `semantiq_index::paths::to_relative_string` for `files.path`, here for
/// `resolved_path`), so a Windows index holds `src/lib/foo.ts` like the
/// documentation, the agents and the `/`-only inputs expect, and
/// `resolved_path` stays comparable to `files.path` everywhere.
pub fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Normalize a path by resolving `.` and `..` components without touching the
/// filesystem. `None` when the path is absolute or a `..` climbs above its
/// start (the project root): such an import names nothing in the project,
/// and `src/../../secret` must not resolve to a root-level `secret` (#83).
fn normalize_path(path: &Path) -> Option<PathBuf> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
            Component::Normal(_) => components.push(component),
        }
    }
    Some(components.iter().collect())
}

/// `import_path` looked up next to the importing file, then under each of
/// `roots` (project-relative directories, `""` for the root itself).
fn path_candidates(source_dir: &Path, import_path: &str, roots: &[&Path]) -> Vec<PathBuf> {
    let mut candidates = vec![source_dir.join(import_path)];
    for root in roots {
        let candidate = root.join(import_path);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// Relative imports (`.x`, `..pkg.mod`) from the importing file's directory;
/// absolute ones (`app.models`) from that directory (a script importing its
/// siblings), the project root and its `src/` layout directory (#81).
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
        let rel = PathBuf::from(import_path.replace('.', "/"));
        path_candidates(
            source_dir,
            &slash_path(&rel),
            &[Path::new(""), Path::new("src")],
        )
    }
}

/// The module a Rust file holds: its directory of child modules and whether
/// it is a crate root (`lib.rs`, `main.rs`, `src/bin/x.rs`, `tests/x.rs`...).
struct RustModule {
    /// Directory holding the file's child modules: the file's own directory
    /// for `mod.rs` and crate roots, `<dir>/<stem>/` otherwise.
    dir: PathBuf,
    crate_root: bool,
}

impl RustModule {
    fn of(source_rel_path: &str) -> Option<Self> {
        let file = Path::new(source_rel_path);
        let dir = file.parent().unwrap_or(Path::new(""));
        let stem = file.file_stem()?.to_str()?;
        let parent_name = dir.file_name().and_then(|n| n.to_str());
        let in_src = find_rust_src_dir(source_rel_path).is_some();
        let crate_root = matches!(stem, "lib" | "main")
            || parent_name == Some("bin")
            || (matches!(parent_name, Some("tests" | "examples" | "benches")) && !in_src);
        let dir = if crate_root || stem == "mod" {
            dir.to_path_buf()
        } else {
            dir.join(stem)
        };
        Some(Self { dir, crate_root })
    }
}

/// Whether `dir` holds the child modules of a crate root: the crate's `src/`
/// (`lib.rs` / `main.rs`), a `src/bin/<name>/` or an integration test
/// directory. `super` from such a module leads nowhere.
fn rust_dir_is_crate_root(dir: &Path, src_dir: Option<&str>) -> bool {
    if src_dir.is_some_and(|src| Path::new(src) == dir) {
        return true;
    }
    let parent_name = dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    parent_name == Some("bin")
        || (matches!(parent_name, Some("tests" | "examples" | "benches")) && src_dir.is_none())
}

/// Files that may hold the module `segments` under `dir` (`dir/a/b.rs`,
/// `dir/a/b/mod.rs`, and the same for the path minus its last segment, which
/// may be an item). With no segment, the file of the module `dir` itself
/// (`dir/mod.rs`, `<dir>.rs`, or the crate root files when `crate_root`).
fn rust_module_candidates(dir: &Path, segments: &[&str], crate_root: bool) -> Vec<PathBuf> {
    if segments.is_empty() {
        let mut candidates = vec![dir.join("mod.rs")];
        if crate_root {
            candidates.push(dir.join("lib.rs"));
            candidates.push(dir.join("main.rs"));
        } else if let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) {
            candidates.push(parent.join(format!("{}.rs", name.to_string_lossy())));
        }
        return candidates;
    }
    let full: PathBuf = dir.join(segments.iter().collect::<PathBuf>());
    let mut candidates = vec![full.join("mod.rs"), full];
    if segments.len() > 1 {
        let module: PathBuf = dir.join(segments[..segments.len() - 1].iter().collect::<PathBuf>());
        candidates.push(module.join("mod.rs"));
        candidates.push(module);
    }
    candidates
}

/// `crate::` paths from the crate's `src/`, `self::` paths and bare module
/// paths (`mod util;`, `use util::f`) from the file's own module directory,
/// `super::` paths from the parent module's (#143); a file path (`#[path =
/// "x/y.rs"] mod m;`) from the file's directory. Glob and brace paths are
/// left to the extractor, which expands them.
fn resolve_rust_import(source_rel_path: &str, import_path: &str) -> Option<Vec<PathBuf>> {
    if import_path.contains('{') || import_path.contains('*') {
        return None;
    }
    let file_dir = Path::new(source_rel_path).parent().unwrap_or(Path::new(""));
    if import_path.contains('/') || import_path.ends_with(".rs") {
        return Some(vec![file_dir.join(import_path)]);
    }

    let segments: Vec<&str> = import_path.split("::").collect();
    let src_dir = find_rust_src_dir(source_rel_path);
    let module = RustModule::of(source_rel_path)?;

    let (dir, crate_root, rest): (PathBuf, bool, &[&str]) = match segments[0] {
        "crate" => (PathBuf::from(src_dir.as_deref()?), true, &segments[1..]),
        // `self` alone is the file itself, not a dependency.
        "self" if segments.len() == 1 => return None,
        "self" => (module.dir, module.crate_root, &segments[1..]),
        "super" => {
            let mut dir = module.dir;
            let mut crate_root = module.crate_root;
            let mut consumed = 0;
            while segments.get(consumed) == Some(&"super") {
                if crate_root {
                    return None;
                }
                dir = dir.parent()?.to_path_buf();
                crate_root = rust_dir_is_crate_root(&dir, src_dir.as_deref());
                consumed += 1;
            }
            (dir, crate_root, &segments[consumed..])
        }
        _ => (module.dir, module.crate_root, &segments[..]),
    };
    Some(rust_module_candidates(&dir, rest, crate_root))
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
    // Bundler suffixes (`./x.ts?raw`, `./a.css?url`, `./b#frag`) are not part of the file name.
    let import_path = import_path.split(['?', '#']).next().unwrap_or(import_path);
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
        Language::Cpp => &["cpp", "cc", "cxx", "hpp", "hxx", "hh", "h"],
        Language::Php => &["php"],
        Language::Ruby => &["rb"],
        Language::CSharp => &["cs"],
        Language::Kotlin => &["kt", "kts"],
        Language::Scala => &["scala", "sc"],
        Language::Bash => &["sh", "bash"],
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
    fn test_resolve_explicit_js_to_ts() {
        let dir = setup_project(&["src/app.ts", "src/foo.ts"]);
        let resolved =
            resolve_local_import("src/app.ts", "./foo.js", Language::TypeScript, dir.path());
        assert_eq!(resolved.as_deref(), Some("src/foo.ts"));
    }

    #[test]
    fn test_resolve_explicit_jsx_to_tsx() {
        let dir = setup_project(&["src/app.ts", "src/foo.tsx"]);
        let resolved =
            resolve_local_import("src/app.ts", "./foo.jsx", Language::TypeScript, dir.path());
        assert_eq!(resolved.as_deref(), Some("src/foo.tsx"));
    }

    #[test]
    fn test_resolve_explicit_mjs_to_ts() {
        let dir = setup_project(&["src/app.ts", "src/foo.ts"]);
        let resolved =
            resolve_local_import("src/app.ts", "./foo.mjs", Language::TypeScript, dir.path());
        assert_eq!(resolved.as_deref(), Some("src/foo.ts"));
    }

    #[test]
    fn test_resolve_exact_js_file_takes_precedence() {
        let dir = setup_project(&["src/app.ts", "src/foo.js", "src/foo.ts"]);
        let resolved =
            resolve_local_import("src/app.ts", "./foo.js", Language::TypeScript, dir.path());
        assert_eq!(resolved.as_deref(), Some("src/foo.js"));
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
    fn test_resolve_dotted_module_name() {
        let dir = setup_project(&["src/app.ts", "src/user.service.ts"]);
        let resolved = resolve_local_import(
            "src/app.ts",
            "./user.service",
            Language::TypeScript,
            dir.path(),
        );
        assert_eq!(resolved.as_deref(), Some("src/user.service.ts"));
    }

    #[test]
    fn test_resolve_dotted_module_not_confused_with_shorter_name() {
        let dir = setup_project(&["src/app.ts", "src/user.ts", "src/user.service.ts"]);
        let resolved = resolve_local_import(
            "src/app.ts",
            "./user.service",
            Language::TypeScript,
            dir.path(),
        );
        assert_eq!(resolved.as_deref(), Some("src/user.service.ts"));
    }

    #[test]
    fn test_resolve_js_query_suffix_is_ignored() {
        let dir = setup_project(&["src/app.ts", "src/registry.ts", "src/styles.css"]);
        let resolved = resolve_local_import(
            "src/app.ts",
            "./registry.ts?raw",
            Language::TypeScript,
            dir.path(),
        );
        assert_eq!(resolved.as_deref(), Some("src/registry.ts"));
        let resolved = resolve_local_import(
            "src/app.ts",
            "./styles.css?url",
            Language::TypeScript,
            dir.path(),
        );
        assert_eq!(resolved.as_deref(), Some("src/styles.css"));
        let resolved = resolve_local_import(
            "src/app.ts",
            "./registry#x",
            Language::TypeScript,
            dir.path(),
        );
        assert_eq!(resolved.as_deref(), Some("src/registry.ts"));
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

    /// #81: absolute imports of the project's own packages resolve from the
    /// project root, from `src/` (src layout) and from the importing file's
    /// directory (a script importing a sibling module); a name that is no
    /// file of the project (a third-party package) does not resolve.
    #[test]
    fn test_resolve_python_absolute_import() {
        let dir = setup_project(&[
            "app/__init__.py",
            "app/models.py",
            "app/views.py",
            "src/pkg/core.py",
            "scripts/run.py",
            "scripts/utils.py",
            "tests/test_models.py",
        ]);
        let resolve =
            |from: &str, path: &str| resolve_local_import(from, path, Language::Python, dir.path());
        assert_eq!(
            resolve("app/views.py", "app.models").as_deref(),
            Some("app/models.py")
        );
        assert_eq!(
            resolve("tests/test_models.py", "app.models").as_deref(),
            Some("app/models.py")
        );
        assert_eq!(
            resolve("app/views.py", "app").as_deref(),
            Some("app/__init__.py")
        );
        assert_eq!(
            resolve("app/views.py", "pkg.core").as_deref(),
            Some("src/pkg/core.py")
        );
        assert_eq!(
            resolve("scripts/run.py", "utils").as_deref(),
            Some("scripts/utils.py")
        );
        assert_eq!(resolve("app/views.py", "numpy"), None);
        assert_eq!(resolve("app/views.py", "numpy.linalg"), None);
    }

    #[test]
    fn test_resolve_rust_crate_import() {
        let dir = setup_project(&["src/utils.rs"]);

        let resolved =
            resolve_local_import("src/main.rs", "crate::utils", Language::Rust, dir.path());
        assert_eq!(resolved.as_deref(), Some("src/utils.rs"));
    }

    /// #143: `mod x;` declarations (bare module paths), `self::` and `super::`
    /// paths resolve from the module tree the file layout defines.
    #[test]
    fn test_resolve_rust_module_paths() {
        let dir = setup_project(&[
            "src/lib.rs",
            "src/util.rs",
            "src/other.rs",
            "src/sub/mod.rs",
            "src/sub/leaf.rs",
            "src/store/mod.rs",
            "src/store/files.rs",
            "src/store/files/io.rs",
            "src/shapes/circle.rs",
            "src/bin/tool/main.rs",
            "src/bin/tool/cli.rs",
            "src/bin/quick.rs",
            "src/bin/flags.rs",
            "tests/common/mod.rs",
            "tests/smoke.rs",
        ]);
        let resolve =
            |from: &str, path: &str| resolve_local_import(from, path, Language::Rust, dir.path());

        // `mod x;` in a crate root or a `mod.rs`: `<dir>/x.rs` or `<dir>/x/mod.rs`.
        assert_eq!(
            resolve("src/lib.rs", "util").as_deref(),
            Some("src/util.rs")
        );
        assert_eq!(
            resolve("src/lib.rs", "sub").as_deref(),
            Some("src/sub/mod.rs")
        );
        assert_eq!(
            resolve("src/sub/mod.rs", "leaf").as_deref(),
            Some("src/sub/leaf.rs")
        );
        // `mod x;` in a non-root file: `<dir>/<stem>/x.rs`.
        assert_eq!(
            resolve("src/store/files.rs", "io").as_deref(),
            Some("src/store/files/io.rs")
        );
        // Crate roots hold their child modules in their own directory.
        assert_eq!(
            resolve("src/bin/tool/main.rs", "cli").as_deref(),
            Some("src/bin/tool/cli.rs")
        );
        assert_eq!(
            resolve("src/bin/quick.rs", "flags").as_deref(),
            Some("src/bin/flags.rs")
        );
        assert_eq!(
            resolve("tests/smoke.rs", "common").as_deref(),
            Some("tests/common/mod.rs")
        );
        // `#[path = "shapes/circle.rs"] mod circle;`
        assert_eq!(
            resolve("src/lib.rs", "shapes/circle.rs").as_deref(),
            Some("src/shapes/circle.rs")
        );
        // `self::`: the file's own module directory, the last segment may be an item.
        assert_eq!(
            resolve("src/lib.rs", "self::other::X").as_deref(),
            Some("src/other.rs")
        );
        assert_eq!(
            resolve("src/sub/mod.rs", "self::leaf").as_deref(),
            Some("src/sub/leaf.rs")
        );
        assert_eq!(resolve("src/lib.rs", "self"), None, "the file itself");
        // `super::`: the parent module directory, the parent module's own file.
        assert_eq!(
            resolve("src/sub/mod.rs", "super::other::X").as_deref(),
            Some("src/other.rs")
        );
        assert_eq!(
            resolve("src/sub/leaf.rs", "super").as_deref(),
            Some("src/sub/mod.rs")
        );
        assert_eq!(
            resolve("src/store/files/io.rs", "super").as_deref(),
            Some("src/store/files.rs")
        );
        assert_eq!(
            resolve("src/store/files/io.rs", "super::super::mod_x"),
            None,
            "no such module"
        );
        assert_eq!(
            resolve("src/sub/leaf.rs", "super::super").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            resolve("src/sub/leaf.rs", "super::super::util::f").as_deref(),
            Some("src/util.rs")
        );
        // The crate root has no parent module.
        assert_eq!(resolve("src/lib.rs", "super::other"), None);
        assert_eq!(resolve("src/bin/tool/cli.rs", "super::super::x"), None);
        // A bare path that is no module of the file stays unresolved (an
        // external crate), `crate::` keeps working.
        assert_eq!(resolve("src/sub/leaf.rs", "anyhow::Result"), None);
        assert_eq!(
            resolve("src/sub/leaf.rs", "crate::util").as_deref(),
            Some("src/util.rs")
        );
        assert_eq!(
            resolve("src/sub/leaf.rs", "crate").as_deref(),
            Some("src/lib.rs")
        );
    }

    /// #142: C includes and Ruby requires are relative to the including file
    /// without a leading `./`; C also looks from the root and `include/`,
    /// Ruby from `lib/`.
    #[test]
    fn test_resolve_c_include_and_ruby_require() {
        let dir = setup_project(&[
            "src/main.c",
            "src/util.h",
            "inc/other.h",
            "include/api.h",
            "lib/app.rb",
            "lib/helpers.rb",
            "lib/app/models.rb",
            "lib/other.rb",
        ]);
        let c = |from: &str, path: &str| resolve_local_import(from, path, Language::C, dir.path());
        assert_eq!(c("src/main.c", "util.h").as_deref(), Some("src/util.h"));
        assert_eq!(c("src/main.c", "./util.h").as_deref(), Some("src/util.h"));
        assert_eq!(
            c("src/main.c", "../inc/other.h").as_deref(),
            Some("inc/other.h")
        );
        assert_eq!(c("src/main.c", "api.h").as_deref(), Some("include/api.h"));
        assert_eq!(
            c("src/main.c", "inc/other.h").as_deref(),
            Some("inc/other.h")
        );
        assert_eq!(c("src/main.c", "missing.h"), None);
        assert_eq!(c("src/main.c", "/usr/include/stdio.h"), None, "absolute");

        let rb =
            |from: &str, path: &str| resolve_local_import(from, path, Language::Ruby, dir.path());
        assert_eq!(
            rb("lib/app.rb", "helpers").as_deref(),
            Some("lib/helpers.rb")
        );
        assert_eq!(rb("lib/app.rb", "./other").as_deref(), Some("lib/other.rb"));
        assert_eq!(rb("lib/app/models.rb", "app/models"), None, "itself");
        assert_eq!(
            rb("lib/app.rb", "app/models").as_deref(),
            Some("lib/app/models.rb")
        );
        assert_eq!(rb("lib/app.rb", "json"), None);
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

    /// #68: `resolved_path` uses `/` on every platform. `Path::join` inserts the
    /// native separator, so this exercises the conversion wherever it runs.
    #[test]
    fn test_slash_path_uses_forward_slashes() {
        let native = Path::new("src").join("lib").join("foo.ts");
        assert!(native.to_string_lossy().contains(std::path::MAIN_SEPARATOR));
        assert_eq!(slash_path(&native), "src/lib/foo.ts");
        assert_eq!(slash_path(Path::new("foo.ts")), "foo.ts");
        assert_eq!(slash_path(Path::new("")), "");
    }

    #[cfg(windows)]
    #[test]
    fn test_slash_path_windows_backslashes() {
        assert_eq!(slash_path(Path::new(r"src\lib\foo.ts")), "src/lib/foo.ts");
    }

    #[test]
    fn test_resolved_paths_use_forward_slashes() {
        let dir = setup_project(&["src/components/Button.tsx", "src/utils/helpers.ts"]);
        let resolved = resolve_local_import(
            "src/components/Button.tsx",
            "../utils/helpers",
            Language::TypeScript,
            dir.path(),
        )
        .unwrap();
        assert!(!resolved.contains('\\'), "{resolved}");
        assert_eq!(resolved, "src/utils/helpers.ts");
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(
            normalize_path(Path::new("src/../lib/utils")),
            Some(PathBuf::from("lib/utils"))
        );
        assert_eq!(
            normalize_path(Path::new("./src/./utils")),
            Some(PathBuf::from("src/utils"))
        );
        // #83: climbing above the start is not "the start".
        assert_eq!(normalize_path(Path::new("src/../../secret")), None);
        assert_eq!(normalize_path(Path::new("../x")), None);
        assert_eq!(normalize_path(Path::new("/etc/passwd")), None);
    }

    /// #83: an import climbing above the project root resolves to nothing,
    /// even when a file of the same name exists at the root (or outside it).
    #[test]
    fn test_resolve_blocks_path_traversal() {
        let outer = TempDir::new().unwrap();
        // A "secret" file outside the project root
        fs::write(outer.path().join("secret.ts"), "// secret").unwrap();

        let project_root = outer.path().join("project");
        fs::create_dir_all(project_root.join("src")).unwrap();
        fs::write(project_root.join("src/app.ts"), "// app").unwrap();
        // ... and a homonym at the project root, which `../../secret` from
        // `src/` does not name either.
        fs::write(project_root.join("secret.ts"), "// root homonym").unwrap();

        for import in ["../../secret", "../../../../secret"] {
            let resolved =
                resolve_local_import("src/app.ts", import, Language::TypeScript, &project_root);
            assert!(
                resolved.is_none(),
                "{import}: imports escaping project_root must not resolve, got {:?}",
                resolved
            );
        }
        // One level up is the root itself: that one is the homonym.
        let resolved = resolve_local_import(
            "src/app.ts",
            "../secret",
            Language::TypeScript,
            &project_root,
        );
        assert_eq!(resolved.as_deref(), Some("secret.ts"));
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

    /// #147: package imports resolve through the packages the project walk
    /// recorded in the `ImportContext`.
    #[test]
    fn test_resolve_package_imports_through_context() {
        let dir = setup_project(&[
            "core/src/main/java/com/acme/models/User.java",
            "core/src/main/java/com/acme/models/Order.java",
            "api/src/main/java/com/acme/api/Server.java",
            "src/main/kotlin/com/acme/kt/Models.kt",
            "src/App/Models/User.php",
            "src/App/Services/Svc.php",
            "Services/Svc.cs",
            "Models/All.cs",
            "internal/store/store.go",
            "internal/store/sqlite.go",
            "internal/store/store_test.go",
            "cmd/app/main.go",
            "pkg/util/doc.go",
            "pkg/util/strings.go",
        ]);
        let mut context = ImportContext::default();
        let add = |context: &mut ImportContext, rel: &str, head: &str| {
            context.add_source_file(rel, Language::from_path(Path::new(rel)).unwrap(), head);
        };
        add(
            &mut context,
            "core/src/main/java/com/acme/models/User.java",
            "package com.acme.models;\n",
        );
        add(
            &mut context,
            "core/src/main/java/com/acme/models/Order.java",
            "package com.acme.models;\n",
        );
        add(
            &mut context,
            "api/src/main/java/com/acme/api/Server.java",
            "package com.acme.api;\n",
        );
        add(
            &mut context,
            "src/main/kotlin/com/acme/kt/Models.kt",
            "package com.acme.kt\n",
        );
        add(
            &mut context,
            "src/App/Models/User.php",
            "<?php\nnamespace App\\Models;\n",
        );
        add(
            &mut context,
            "src/App/Services/Svc.php",
            "<?php\nnamespace App\\Services;\n",
        );
        add(&mut context, "Services/Svc.cs", "namespace App.Services;\n");
        add(
            &mut context,
            "Models/All.cs",
            "namespace App.Models\n{\n}\n",
        );
        for go in [
            "internal/store/store.go",
            "internal/store/sqlite.go",
            "internal/store/store_test.go",
            "cmd/app/main.go",
            "pkg/util/doc.go",
            "pkg/util/strings.go",
        ] {
            add(&mut context, go, "");
        }

        let resolve = |context: &ImportContext, from: &str, path: &str| {
            let language = Language::from_path(Path::new(from)).unwrap();
            resolve_import(from, path, language, dir.path(), context)
        };
        // Java: the file named after the class, across modules.
        assert_eq!(
            resolve(
                &context,
                "api/src/main/java/com/acme/api/Server.java",
                "com.acme.models.User"
            )
            .as_deref(),
            Some("core/src/main/java/com/acme/models/User.java")
        );
        // `import static com.acme.models.Order.total` / wildcard of a multi-file package.
        assert_eq!(
            resolve(
                &context,
                "api/src/main/java/com/acme/api/Server.java",
                "com.acme.models.Order.total"
            )
            .as_deref(),
            Some("core/src/main/java/com/acme/models/Order.java")
        );
        assert_eq!(
            resolve(
                &context,
                "api/src/main/java/com/acme/api/Server.java",
                "com.acme.models"
            ),
            None
        );
        assert_eq!(
            resolve(
                &context,
                "api/src/main/java/com/acme/api/Server.java",
                "com.google.common.base.Strings"
            ),
            None
        );
        // Kotlin: a top-level declaration in a file named differently, single file of its package.
        assert_eq!(
            resolve(
                &context,
                "api/src/main/java/com/acme/api/Server.java",
                "com.acme.kt.User"
            )
            .as_deref(),
            Some("src/main/kotlin/com/acme/kt/Models.kt")
        );
        // PHP
        assert_eq!(
            resolve(&context, "src/App/Services/Svc.php", "App\\Models\\User").as_deref(),
            Some("src/App/Models/User.php")
        );
        assert_eq!(
            resolve(
                &context,
                "src/App/Services/Svc.php",
                "Illuminate\\Support\\Str"
            ),
            None
        );
        // C#: `using App.Models;` names the namespace, one file declares it.
        assert_eq!(
            resolve(&context, "Services/Svc.cs", "App.Models").as_deref(),
            Some("Models/All.cs")
        );
        assert_eq!(resolve(&context, "Services/Svc.cs", "System.Linq"), None);
        // Go without go.mod: a suffix of the import path is a package directory;
        // the package file is `<dir>.go`, else `doc.go`, else the first file.
        assert_eq!(
            resolve(&context, "cmd/app/main.go", "myapp/internal/store").as_deref(),
            Some("internal/store/store.go")
        );
        assert_eq!(
            resolve(&context, "cmd/app/main.go", "myapp/pkg/util").as_deref(),
            Some("pkg/util/doc.go")
        );
        assert_eq!(resolve(&context, "cmd/app/main.go", "net/http"), None);
        assert_eq!(
            resolve(&context, "cmd/app/main.go", "github.com/x/store"),
            None
        );
        // With go.mod, only paths under the module match.
        context.go_module = Some("example.com/myapp".to_string());
        assert_eq!(
            resolve(
                &context,
                "cmd/app/main.go",
                "example.com/myapp/internal/store"
            )
            .as_deref(),
            Some("internal/store/store.go")
        );
        assert_eq!(
            resolve(&context, "cmd/app/main.go", "myapp/internal/store"),
            None
        );
    }
}
