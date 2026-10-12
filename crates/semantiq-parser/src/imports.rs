use crate::language::Language;
use crate::python_stdlib::PYTHON_STD_MODULES;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone)]
pub struct Import {
    pub path: String,
    pub name: Option<String>,
    pub kind: ImportKind,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// Standard library import
    Std,
    /// External crate/package
    External,
    /// Local/relative import
    Local,
}

impl ImportKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImportKind::Std => "std",
            ImportKind::External => "external",
            ImportKind::Local => "local",
        }
    }
}

impl std::fmt::Display for ImportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ImportKind {
    type Err = ();

    /// Inverse de [`ImportKind::as_str`]. Insensible à la casse. Renvoie
    /// `Err(())` pour une valeur inconnue.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "std" => Ok(ImportKind::Std),
            "external" => Ok(ImportKind::External),
            "local" => Ok(ImportKind::Local),
            _ => Err(()),
        }
    }
}

/// Project-level facts that change how imports are classified and resolved:
/// the `go.mod` module path and the packages the project's own files declare
/// (#147). Built once per indexer, from the project walk
/// (`add_source_file`); a package added to the project afterwards is seen by
/// the next indexer, like a `go.mod` edit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportContext {
    /// `module` path of the project's `go.mod`: imports under it are `Local`
    /// (without it, `myapp/internal/store` looks like a standard package).
    pub go_module: Option<String>,
    /// Packages / namespaces declared by the project's files, as written in
    /// the language (`com.acme.app` for Java, Kotlin, Scala and C#,
    /// `App\Models` for PHP), to the relative paths of the files declaring
    /// them, sorted.
    pub local_packages: BTreeMap<String, Vec<String>>,
    /// Directories holding Go files (relative, `/`-separated, the root as
    /// `""`), to their non-test `.go` files, sorted.
    pub go_packages: BTreeMap<String, Vec<String>>,
}

/// Standard-library roots that have sub-packages (`net/http`,
/// `encoding/json`): without a `go.mod`, a project directory `http` must not
/// claim an `import "net/http"`.
const GO_STD_ROOTS: &[&str] = &[
    "archive",
    "compress",
    "container",
    "crypto",
    "database",
    "debug",
    "encoding",
    "go",
    "hash",
    "html",
    "image",
    "index",
    "io",
    "log",
    "math",
    "mime",
    "net",
    "os",
    "path",
    "regexp",
    "runtime",
    "sync",
    "testing",
    "text",
    "unicode",
];

impl ImportContext {
    /// Reads the facts from the files at the project root (`go.mod`). The
    /// declared packages come from the project walk (`add_source_file`).
    pub fn from_project_root(project_root: &Path) -> Self {
        Self {
            go_module: go_module_path(project_root),
            ..Self::default()
        }
    }

    /// Whether files of `language` declare the packages `add_source_file`
    /// records: Java, Kotlin, Scala, C# and PHP (a declaration in the file
    /// head), Go (the file's directory).
    pub fn records_packages(language: Language) -> bool {
        matches!(
            language,
            Language::Java
                | Language::Kotlin
                | Language::Scala
                | Language::CSharp
                | Language::Php
                | Language::Go
        )
    }

    /// Records the packages the file `rel_path` (relative to the project
    /// root, `/`-separated) declares: its directory for a Go file (test files
    /// are skipped), the `package` / `namespace` declarations found in `head`
    /// otherwise. `head` is the beginning of the file: declarations come
    /// before any code, the first few KiB are enough.
    pub fn add_source_file(&mut self, rel_path: &str, language: Language, head: &str) {
        if language == Language::Go {
            if rel_path.ends_with("_test.go") {
                return;
            }
            let dir = rel_path.rsplit_once('/').map_or("", |(dir, _)| dir);
            Self::record(&mut self.go_packages, dir, rel_path);
            return;
        }
        for package in Self::declared_packages(language, head) {
            Self::record(&mut self.local_packages, &package, rel_path);
        }
    }

    fn record(map: &mut BTreeMap<String, Vec<String>>, key: &str, rel_path: &str) {
        let files = map.entry(key.to_string()).or_default();
        if let Err(at) = files.binary_search_by(|f| f.as_str().cmp(rel_path)) {
            files.insert(at, rel_path.to_string());
        }
    }

    /// The `package a.b.c` (Java, Kotlin, Scala: chained Scala clauses give
    /// `a.b` and `a.b.c`) or `namespace A.B` / `namespace A\B` (C#, PHP)
    /// declarations in `head`, read as text: a declaration at the start of a
    /// line, followed by the name.
    pub fn declared_packages(language: Language, head: &str) -> Vec<String> {
        let keyword = match language {
            Language::Java | Language::Kotlin | Language::Scala => "package",
            Language::CSharp | Language::Php => "namespace",
            _ => return Vec::new(),
        };
        let mut packages: Vec<String> = Vec::new();
        let mut scala_chain: Option<String> = None;
        for line in head.lines() {
            let Some(rest) = line.trim_start().strip_prefix(keyword) else {
                continue;
            };
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\\'))
                .collect();
            let name = name.trim_matches(['.', '\\']);
            // `package object x` (Scala) declares no package of its own.
            if name.is_empty() || name == "object" {
                continue;
            }
            let full = match (&scala_chain, language) {
                (Some(chain), Language::Scala) => format!("{chain}.{name}"),
                _ => name.to_string(),
            };
            if language == Language::Scala {
                scala_chain = Some(full.clone());
            }
            if !packages.contains(&full) {
                packages.push(full);
            }
        }
        packages
    }

    /// The longest package declared by files of the ecosystem of `language`
    /// that `path` names or lies in, with those files: the path itself
    /// (`using App.Models;`, a wildcard), a member of it (`a.b.C`) or a member
    /// of a member whose file is named after the first (`import static
    /// a.b.C.m`: `C.java`). A longer undeclared prefix (`a.b.models.X` when
    /// only `a.b` is declared) is another package of the same group: not
    /// local. A C# namespace and a JVM package may share a name in one
    /// repository: a `using` only sees `.cs` files, a Java / Kotlin / Scala
    /// import only JVM files.
    fn package_of(&self, path: &str, language: Language) -> Option<(usize, Vec<&String>)> {
        let (sep, extensions) = package_ecosystem(language)?;
        let segments: Vec<&str> = path.split(sep).collect();
        let sep = sep.to_string();
        for k in (1..=segments.len()).rev() {
            let Some(declaring) = self.local_packages.get(&segments[..k].join(&sep)) else {
                continue;
            };
            let files: Vec<&String> = declaring
                .iter()
                .filter(|f| extensions.contains(&file_extension(f)))
                .collect();
            if files.is_empty() {
                continue;
            }
            let fits = match segments.len() - k {
                0 | 1 => true,
                2 => files.iter().any(|f| file_stem(f) == segments[k]),
                _ => false,
            };
            return fits.then_some((k, files));
        }
        None
    }

    /// Whether `path`, an import written in `language`, names a package of
    /// the project or a member of one.
    pub fn is_local_package(&self, path: &str, language: Language) -> bool {
        self.package_of(path, language).is_some()
    }

    /// The file of the project defining `path`, an import written in
    /// `language`: the file of the package named after the imported member
    /// (`com.acme.models.User` → `User.java`), else, for the package itself
    /// or a member whose file is named differently (Kotlin / Scala top-level
    /// declarations, C# namespaces), the single file declaring the package,
    /// if there is one.
    pub fn resolve_package_import(&self, path: &str, language: Language) -> Option<String> {
        let (k, files) = self.package_of(path, language)?;
        let (sep, _) = package_ecosystem(language)?;
        let segments: Vec<&str> = path.split(sep).collect();
        if let Some(member) = segments.get(k)
            && let Some(file) = files.iter().find(|f| file_stem(f) == *member)
        {
            return Some((*file).clone());
        }
        (segments.len() - k <= 1 && files.len() == 1).then(|| files[0].clone())
    }

    /// The directory of the project package a Go import path names: the path
    /// under the `go.mod` module, or without a `go.mod` (GOPATH layout, the
    /// import path starts with the project's name) a proper suffix of the
    /// path. Hosted paths (`github.com/...`) and standard roots are never
    /// project packages.
    pub fn go_package_dir(&self, import_path: &str) -> Option<&str> {
        let key = |dir: &str| self.go_packages.get_key_value(dir).map(|(k, _)| k.as_str());
        if let Some(module) = &self.go_module {
            // The module itself (`import "example.com/myapp"`) is the root
            // package, recorded under `""`.
            return match import_path.strip_prefix(module.as_str())? {
                "" => key(""),
                rest => key(rest.strip_prefix('/')?),
            };
        }
        let (first, mut rest) = import_path.split_once('/')?;
        if first.contains('.') || GO_STD_ROOTS.contains(&first) {
            return None;
        }
        loop {
            if let Some(found) = key(rest) {
                return Some(found);
            }
            rest = rest.split_once('/')?.1;
        }
    }

    /// The file standing for the Go package `import_path` names:
    /// `<dir>/<dir name>.go` (for the root package, the last segment of the
    /// module path), else `doc.go`, else the first non-test file.
    pub fn resolve_go_import(&self, import_path: &str) -> Option<String> {
        let dir = self.go_package_dir(import_path)?;
        let files = self.go_packages.get(dir)?;
        let name = if dir.is_empty() {
            self.go_module
                .as_deref()
                .map(|module| module.rsplit('/').next().unwrap_or(module))
                .unwrap_or("")
        } else {
            dir.rsplit('/').next().unwrap_or(dir)
        };
        files
            .iter()
            .find(|f| file_stem(f) == name)
            .or_else(|| files.iter().find(|f| file_stem(f) == "doc"))
            .or_else(|| files.first())
            .cloned()
    }
}

/// `a/b/Name.ext` → `Name`.
fn file_stem(rel_path: &str) -> &str {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// `a/b/Name.ext` → `ext` (`""` without one).
fn file_extension(rel_path: &str) -> &str {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    name.rsplit_once('.').map_or("", |(_, ext)| ext)
}

/// The package separator of `language` and the extensions of the files
/// whose declarations its imports can name: the JVM languages see each
/// other's packages, C# and PHP only their own.
fn package_ecosystem(language: Language) -> Option<(char, &'static [&'static str])> {
    match language {
        Language::Java | Language::Kotlin | Language::Scala => {
            Some(('.', &["java", "kt", "kts", "scala"]))
        }
        Language::CSharp => Some(('.', &["cs"])),
        Language::Php => Some(('\\', &["php"])),
        _ => None,
    }
}

/// The `module` directive of `<project_root>/go.mod`, if any.
pub fn go_module_path(project_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(project_root.join("go.mod")).ok()?;
    text.lines().find_map(|line| {
        let module = line.trim().strip_prefix("module")?;
        if !module.starts_with(char::is_whitespace) {
            return None;
        }
        let module = module.split("//").next()?.trim().trim_matches('"');
        (!module.is_empty()).then(|| module.to_string())
    })
}

pub struct ImportExtractor;

impl ImportExtractor {
    pub fn extract(tree: &Tree, source: &str, language: Language) -> Result<Vec<Import>> {
        Self::extract_with_context(tree, source, language, &ImportContext::default())
    }

    /// `extract` with the project facts (`go.mod` module path) that refine the
    /// `ImportKind` classification.
    pub fn extract_with_context(
        tree: &Tree,
        source: &str,
        language: Language,
        context: &ImportContext,
    ) -> Result<Vec<Import>> {
        let mut imports = Vec::new();

        // Pre-order walk with an explicit stack: one native frame per AST
        // level overflowed the 2 MiB stack of a `spawn_blocking` thread on a
        // few thousand nested nodes (#78), and the walk runs on every
        // language, data files included.
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            imports.extend(Self::node_to_import(&node, source, language, context));
            let first_child = stack.len();
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
            // Children were pushed in source order: reverse them so the next
            // pop visits the first one (source order in `imports`).
            stack[first_child..].reverse();
        }

        Ok(imports)
    }

    fn node_to_import(
        node: &Node,
        source: &str,
        language: Language,
        context: &ImportContext,
    ) -> Vec<Import> {
        match language {
            Language::Rust => Self::extract_rust_import(node, source),
            Language::TypeScript | Language::JavaScript => {
                Self::extract_ts_import(node, source).into_iter().collect()
            }
            Language::Python => Self::extract_python_import(node, source),
            Language::Go => Self::extract_go_import(node, source, context)
                .into_iter()
                .collect(),
            Language::Java => Self::extract_java_import(node, source, context)
                .into_iter()
                .collect(),
            Language::C | Language::Cpp => {
                Self::extract_c_import(node, source).into_iter().collect()
            }
            Language::Php => Self::extract_php_import(node, source, context),
            Language::Ruby => Self::extract_ruby_import(node, source)
                .into_iter()
                .collect(),
            Language::CSharp => Self::extract_csharp_import(node, source, context)
                .into_iter()
                .collect(),
            Language::Kotlin => Self::extract_kotlin_import(node, source, context)
                .into_iter()
                .collect(),
            Language::Scala => Self::extract_scala_import(node, source, context),
            // Markup/config languages don't have traditional imports
            Language::Html | Language::Json | Language::Yaml | Language::Toml => Vec::new(),
            Language::Bash => Self::extract_bash_import(node, source)
                .into_iter()
                .collect(),
            Language::Elixir => Self::extract_elixir_import(node, source),
        }
    }

    /// `use` declarations, one `Import` per leaf of a brace group, and module
    /// declarations without a body (`mod x;`, `#[path = "a/b.rs"] mod x;`):
    /// the way a crate declares its file tree, a `Local` import of `x` (or of
    /// the attribute's file path) named `x` (#143). Inside an inline module
    /// (`mod outer { mod x; }`), the file lives under `outer/`: the import is
    /// `self::outer::x`, or `outer/<path>` for a `#[path]` attribute.
    fn extract_rust_import(node: &Node, source: &str) -> Vec<Import> {
        if node.kind() == "mod_item" {
            return Self::extract_rust_mod_declaration(node, source)
                .into_iter()
                .collect();
        }
        if node.kind() != "use_declaration" {
            return Vec::new();
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let inline_modules = Self::rust_inline_modules(node, source);

        // Get the full use path text, without the comments a brace group may
        // hold (`use foo::{ // keep\n Bar }`), which are not path segments.
        let Ok(text) = node.utf8_text(source.as_bytes()) else {
            return Vec::new();
        };
        let text = Self::strip_rust_comments(text);

        // Extract the path body from "use path::to::module;" / "pub use ...;"
        let body = match Self::parse_rust_use_path(&text) {
            Some(b) => b,
            None => return Vec::new(),
        };

        // Expand brace groups into one (path, name) leaf per import.
        // `name` is the binding name: alias if present, last segment otherwise,
        // or None for glob imports.
        let mut leaves: Vec<(String, Option<String>)> = Vec::new();
        Self::expand_rust_use("", &body, &mut leaves);

        leaves
            .into_iter()
            .map(|(path, name)| {
                let path = Self::rebase_rust_path(&path, &inline_modules);
                let kind = Self::classify_rust_import(&path);
                Import {
                    path,
                    name,
                    kind,
                    start_line,
                    end_line,
                }
            })
            .collect()
    }

    /// `mod x;`: a `mod_item` without a `body`. The path is the module name,
    /// or the file path of a preceding `#[path = "..."]` attribute, both
    /// under the enclosing inline modules.
    fn extract_rust_mod_declaration(node: &Node, source: &str) -> Option<Import> {
        if node.child_by_field_name("body").is_some() {
            return None;
        }
        let src = source.as_bytes();
        let name = node
            .child_by_field_name("name")?
            .utf8_text(src)
            .ok()?
            .to_string();
        let mut path: Option<String> = None;
        let mut previous = node.prev_sibling();
        while let Some(sibling) = previous {
            if sibling.kind() != "attribute_item" {
                break;
            }
            if let Some(attribute) = sibling.named_child(0)
                && attribute.kind() == "attribute"
                && attribute
                    .named_child(0)
                    .and_then(|key| key.utf8_text(src).ok())
                    == Some("path")
                && let Some(value) = attribute.child_by_field_name("value")
                && value.kind() == "string_literal"
                && let Ok(text) = value.utf8_text(src)
            {
                path = Some(text.trim_matches('"').to_string());
            }
            previous = sibling.prev_sibling();
        }
        let inline_modules = Self::rust_inline_modules(node, source);
        let path = match path {
            Some(file) if inline_modules.is_empty() => file,
            Some(file) => format!("{}/{file}", inline_modules.join("/")),
            None if inline_modules.is_empty() => name.clone(),
            None => format!("self::{}::{name}", inline_modules.join("::")),
        };
        Some(Import {
            path,
            name: Some(name),
            kind: ImportKind::Local,
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
        })
    }

    /// Names of the inline modules (`mod tests { ... }`) enclosing `node`,
    /// outermost first.
    fn rust_inline_modules(node: &Node, source: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut parent = node.parent();
        while let Some(ancestor) = parent {
            if ancestor.kind() == "mod_item"
                && let Some(name) = ancestor.child_by_field_name("name")
                && let Ok(text) = name.utf8_text(source.as_bytes())
            {
                names.push(text.to_string());
            }
            parent = ancestor.parent();
        }
        names.reverse();
        names
    }

    /// A `self::` / `super::` path written inside inline modules, rebased on
    /// the module the file holds, which is what the resolver knows: in `mod
    /// tests { use super::helper; }`, `super` is the file's module, so the
    /// path becomes `self::helper`; `use super::super::x` there stays one
    /// `super` up; `use self::x` becomes `self::tests::x`. `crate::` and
    /// bare paths are unchanged.
    fn rebase_rust_path(path: &str, inline_modules: &[String]) -> String {
        if inline_modules.is_empty() {
            return path.to_string();
        }
        let segments: Vec<&str> = path.split("::").collect();
        let supers = segments.iter().take_while(|s| **s == "super").count();
        if supers == 0 && segments[0] != "self" {
            return path.to_string();
        }
        let rest = &segments[supers.max(1)..];
        let mut rebased: Vec<&str> = Vec::new();
        if supers > inline_modules.len() {
            rebased.extend(std::iter::repeat_n("super", supers - inline_modules.len()));
        } else {
            rebased.push("self");
            rebased.extend(
                inline_modules[..inline_modules.len() - supers]
                    .iter()
                    .map(String::as_str),
            );
        }
        rebased.extend(rest);
        rebased.join("::")
    }

    /// Remove `// ...` (to end of line) and `/* ... */` comments. `use` paths
    /// hold no string literals, so no delimiter can be mistaken for a comment.
    fn strip_rust_comments(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("//") {
                rest = after.find('\n').map(|i| &after[i..]).unwrap_or("");
            } else if let Some(after) = rest.strip_prefix("/*") {
                rest = after.find("*/").map(|i| &after[i + 2..]).unwrap_or("");
            } else {
                let mut chars = rest.chars();
                let c = chars.next().unwrap();
                out.push(c);
                rest = chars.as_str();
            }
        }
        out
    }

    fn parse_rust_use_path(text: &str) -> Option<String> {
        // Drops the trailing semicolon, then strips any visibility modifier
        // ("pub", "pub(crate)", "pub(super)", "pub(in path)", ...) before
        // stripping "use ". Without this, `pub use foo::Bar;` and similar
        // re-exports were silently dropped — which broke `deps` on every
        // `lib.rs` of a Rust crate, since those files are mostly `pub use`.
        let text = text.trim().strip_suffix(';')?.trim();

        // Strip "pub" or "pub(...)" prefix if present.
        let text = if let Some(rest) = text.strip_prefix("pub") {
            let rest = rest.trim_start();
            // Optional restricted form: pub(crate), pub(super), pub(in some::path)
            if let Some(after_open) = rest.strip_prefix('(') {
                if let Some(close_idx) = after_open.find(')') {
                    after_open[close_idx + 1..].trim_start()
                } else {
                    rest // malformed; let the parser handle it
                }
            } else {
                rest
            }
        } else {
            text
        };

        let text = text.strip_prefix("use ")?.trim();
        Some(text.to_string())
    }

    /// Recursively expand a Rust `use` path body into a list of (full_path, name) leaves.
    ///
    /// `name` is the binding produced by the import: the alias if present, otherwise
    /// the last path segment. `None` indicates a glob (`foo::*`) — no single binding.
    ///
    /// Examples:
    /// - prefix="" body="std::io::{Read, Write}"
    ///   -> [("std::io::Read", Some("Read")), ("std::io::Write", Some("Write"))]
    /// - prefix="" body="foo::{A as X, B}"
    ///   -> [("foo::A", Some("X")), ("foo::B", Some("B"))]
    /// - prefix="" body="foo::*"
    ///   -> [("foo", None)]  // glob: keep prefix, no name
    /// - prefix="" body="foo::{a::{X, Y}, B}"
    ///   -> [("foo::a::X", Some("X")), ("foo::a::Y", Some("Y")), ("foo::B", Some("B"))]
    fn expand_rust_use(prefix: &str, body: &str, out: &mut Vec<(String, Option<String>)>) {
        let body = body.trim();
        if body.is_empty() {
            return;
        }

        // Find a top-level brace group (depth 0). If present, split into
        // "before::{contents}[::after]" and expand each comma-separated item
        // inside the braces with the new prefix = before.
        if let Some(brace_start) = Self::find_top_level_char(body, '{') {
            // Match the corresponding closing brace.
            let brace_end = match Self::find_matching_brace(body, brace_start) {
                Some(e) => e,
                None => return,
            };

            // Everything before the "{" should end with "::" (or be empty for "use {a, b};").
            let before = body[..brace_start].trim_end_matches("::").trim();
            let inner = &body[brace_start + 1..brace_end];

            let new_prefix = if before.is_empty() {
                prefix.trim_end_matches("::").to_string()
            } else if prefix.is_empty() {
                before.to_string()
            } else {
                format!("{}::{}", prefix.trim_end_matches("::"), before)
            };

            for item in Self::split_top_level_commas(inner) {
                Self::expand_rust_use(&new_prefix, item.trim(), out);
            }
            return;
        }

        // No braces: this is a leaf. Handle "X as Y", "*", "self".
        let (raw_path, alias) = if let Some(idx) = body.find(" as ") {
            (
                body[..idx].trim().to_string(),
                Some(body[idx + 4..].trim().to_string()),
            )
        } else {
            (body.to_string(), None)
        };

        // Glob: "*" alone or trailing "::*" -> keep the path before the glob, name=None.
        if raw_path == "*" {
            if !prefix.is_empty() {
                out.push((prefix.to_string(), None));
            }
            return;
        }
        if let Some(stripped) = raw_path.strip_suffix("::*") {
            let combined = if prefix.is_empty() {
                stripped.to_string()
            } else if stripped.is_empty() {
                prefix.to_string()
            } else {
                format!("{}::{}", prefix, stripped)
            };
            if !combined.is_empty() {
                out.push((combined, None));
            }
            return;
        }

        // "self" alone refers to the prefix module itself: "use foo::{self, Bar};" -> "foo"
        if raw_path == "self" {
            if !prefix.is_empty() {
                let name = alias.or_else(|| prefix.rsplit("::").next().map(String::from));
                out.push((prefix.to_string(), name));
            }
            return;
        }

        let full = if prefix.is_empty() {
            raw_path.clone()
        } else if raw_path.is_empty() {
            prefix.to_string()
        } else {
            format!("{}::{}", prefix, raw_path)
        };

        // Binding name: alias takes precedence, otherwise last segment of the leaf path.
        let name = alias.or_else(|| raw_path.rsplit("::").next().map(String::from));
        out.push((full, name));
    }

    /// Find the first occurrence of `target` at brace depth 0. Returns byte index.
    fn find_top_level_char(s: &str, target: char) -> Option<usize> {
        let mut depth: i32 = 0;
        for (i, c) in s.char_indices() {
            match c {
                '{' => {
                    if c == target && depth == 0 {
                        return Some(i);
                    }
                    depth += 1;
                }
                '}' => depth -= 1,
                _ => {
                    if c == target && depth == 0 {
                        return Some(i);
                    }
                }
            }
        }
        None
    }

    /// Given a `{` position, find the matching `}` byte index.
    fn find_matching_brace(s: &str, open: usize) -> Option<usize> {
        let mut depth: i32 = 0;
        for (i, c) in s.char_indices().skip_while(|(i, _)| *i < open) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Split `s` on commas at brace depth 0.
    fn split_top_level_commas(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut depth: i32 = 0;
        let mut start = 0usize;
        for (i, c) in s.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            }
        }
        if start <= s.len() {
            out.push(&s[start..]);
        }
        out
    }

    fn classify_rust_import(path: &str) -> ImportKind {
        let first_segment = path.split("::").next().unwrap_or(path);

        match first_segment {
            "std" | "core" | "alloc" => ImportKind::Std,
            "crate" | "self" | "super" => ImportKind::Local,
            _ => ImportKind::External,
        }
    }

    /// `import ... from 'x'`, the re-exports of barrel files (`export * from
    /// 'x'`, `export { a } from 'x'`: an `export_statement` with a `source`)
    /// and CommonJS `require('x')` calls. `.`-prefixed paths are `Local`.
    fn extract_ts_import(node: &Node, source: &str) -> Option<Import> {
        let module = match node.kind() {
            "import_statement" => Self::ts_module_string(node),
            "export_statement" => node.child_by_field_name("source"),
            "call_expression" => Self::require_argument(node, source),
            _ => None,
        }?;
        let path_text = module.utf8_text(source.as_bytes()).ok()?;
        let path = path_text
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();

        let kind = if path.starts_with('.') {
            ImportKind::Local
        } else {
            ImportKind::External
        };

        let name = path.split('/').next_back().map(String::from);

        Some(Import {
            path,
            name,
            kind,
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
        })
    }

    /// The `string` child of an `import_statement` (its `source` field).
    fn ts_module_string<'a>(node: &Node<'a>) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        node.children(&mut cursor).find(|c| c.kind() == "string")
    }

    /// The string argument of `require('x')`.
    fn require_argument<'a>(node: &Node<'a>, source: &str) -> Option<Node<'a>> {
        let function = node.child_by_field_name("function")?;
        if function.kind() != "identifier"
            || function.utf8_text(source.as_bytes()).ok()? != "require"
        {
            return None;
        }
        let arguments = node.child_by_field_name("arguments")?;
        if arguments.named_child_count() != 1 {
            return None;
        }
        arguments
            .named_child(0)
            .filter(|arg| arg.kind() == "string")
    }

    /// One `Import` per imported module: `import a, b` and `import numpy as np`
    /// (an `aliased_import` whose `name` field holds the `dotted_name`) each
    /// yield their own entry, the alias being the binding `name`. A `from`
    /// import yields one entry for its module, except `from . import a, b`
    /// where the imported names are the modules: one entry per name, path `.`
    /// (`from . import *`: one entry without a name).
    fn extract_python_import(node: &Node, source: &str) -> Vec<Import> {
        let src = source.as_bytes();
        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let text = |n: Node| n.utf8_text(src).ok().map(str::to_string);
        // `dotted_name` or `aliased_import` → (dotted path, binding name).
        let target = |n: Node| -> Option<(String, Option<String>)> {
            match n.kind() {
                "dotted_name" => {
                    let path = text(n)?;
                    let name = path.split('.').next_back().map(String::from);
                    Some((path, name))
                }
                "aliased_import" => {
                    let path = text(n.child_by_field_name("name")?)?;
                    let alias = n.child_by_field_name("alias").and_then(text);
                    Some((path, alias))
                }
                _ => None,
            }
        };

        match node.kind() {
            "import_statement" => {
                let mut cursor = node.walk();
                node.children_by_field_name("name", &mut cursor)
                    .filter_map(target)
                    .map(|(path, name)| Import {
                        kind: Self::classify_python_import(&path),
                        path,
                        name,
                        start_line,
                        end_line,
                    })
                    .collect()
            }
            "import_from_statement" => {
                let Some(module) = node.child_by_field_name("module_name") else {
                    return Vec::new();
                };
                let Some(path) = text(module) else {
                    return Vec::new();
                };
                let kind = if path.starts_with('.') {
                    ImportKind::Local
                } else {
                    Self::classify_python_import(&path)
                };
                let bare_relative = path.chars().all(|c| c == '.');
                let mut cursor = node.walk();
                let names: Vec<Option<String>> = if bare_relative {
                    // `from . import a, b`: the imported names are the modules;
                    // `from . import *` is a `wildcard_import` child.
                    let mut names: Vec<Option<String>> = node
                        .children_by_field_name("name", &mut cursor)
                        .filter_map(target)
                        .map(|(_, name)| name)
                        .collect();
                    let mut cursor = node.walk();
                    if node
                        .named_children(&mut cursor)
                        .any(|c| c.kind() == "wildcard_import")
                    {
                        names.push(None);
                    }
                    names
                } else {
                    vec![path.split('.').next_back().map(String::from)]
                };
                names
                    .into_iter()
                    .map(|name| Import {
                        path: path.clone(),
                        name,
                        kind,
                        start_line,
                        end_line,
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn classify_python_import(path: &str) -> ImportKind {
        let first_segment = path.split('.').next().unwrap_or(path);

        if PYTHON_STD_MODULES.binary_search(&first_segment).is_ok() {
            ImportKind::Std
        } else {
            ImportKind::External
        }
    }

    fn extract_go_import(node: &Node, source: &str, context: &ImportContext) -> Option<Import> {
        if node.kind() != "import_spec" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the interpreted_string_literal child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "interpreted_string_literal" {
                let path_text = child.utf8_text(source.as_bytes()).ok()?;
                let path = path_text.trim_matches('"').to_string();

                let in_module = context.go_module.as_deref().is_some_and(|module| {
                    path == module
                        || path
                            .strip_prefix(module)
                            .is_some_and(|rest| rest.starts_with('/'))
                });
                let in_project = in_module
                    || (context.go_module.is_none() && context.go_package_dir(&path).is_some());
                let kind = if in_project || path.starts_with('.') || path.starts_with('/') {
                    ImportKind::Local
                } else if path.split('/').next().unwrap_or(&path).contains('.') {
                    // External packages have a host in their first element
                    // (github.com/...); standard packages never do.
                    ImportKind::External
                } else {
                    ImportKind::Std
                };

                let name = path.split('/').next_back().map(String::from);

                return Some(Import {
                    path,
                    name,
                    kind,
                    start_line,
                    end_line,
                });
            }
        }

        None
    }

    fn extract_java_import(node: &Node, source: &str, context: &ImportContext) -> Option<Import> {
        if node.kind() != "import_declaration" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the scoped_identifier child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "scoped_identifier" {
                let path = child.utf8_text(source.as_bytes()).ok()?.to_string();

                let kind = if context.is_local_package(&path, Language::Java) {
                    ImportKind::Local
                } else if path.starts_with("java.") || path.starts_with("javax.") {
                    ImportKind::Std
                } else {
                    ImportKind::External
                };

                let name = path.split('.').next_back().map(String::from);

                return Some(Import {
                    path,
                    name,
                    kind,
                    start_line,
                    end_line,
                });
            }
        }

        None
    }

    fn extract_c_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "preproc_include" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the string_literal or system_lib_string child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "string_literal" => {
                    let path_text = child.utf8_text(source.as_bytes()).ok()?;
                    let path = path_text.trim_matches('"').to_string();
                    let name = path.split('/').next_back().map(String::from);

                    return Some(Import {
                        path,
                        name,
                        kind: ImportKind::Local,
                        start_line,
                        end_line,
                    });
                }
                "system_lib_string" => {
                    let path_text = child.utf8_text(source.as_bytes()).ok()?;
                    let path = path_text.trim_matches(|c| c == '<' || c == '>').to_string();
                    let name = path.split('/').next_back().map(String::from);

                    return Some(Import {
                        path,
                        name,
                        kind: ImportKind::Std,
                        start_line,
                        end_line,
                    });
                }
                _ => {}
            }
        }

        None
    }

    /// `use Foo\Bar;`, `use Foo\Bar as Baz;`, `use A\B, C\D;` and the group
    /// form `use Foo\{A, B as C};`: one `Import` per leaf, the alias (if any)
    /// as its `name`.
    fn extract_php_import(node: &Node, source: &str, context: &ImportContext) -> Vec<Import> {
        if node.kind() != "namespace_use_declaration" {
            return Vec::new();
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let src = source.as_bytes();
        let text = |n: Node| n.utf8_text(src).ok().map(str::to_string);

        // Group form: the prefix is the `namespace_name` child, the leaves are
        // the clauses of the `namespace_use_group` body.
        let mut prefix: Option<String> = None;
        let mut clauses: Vec<Node> = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "namespace_name" => prefix = text(child),
                "namespace_use_clause" => clauses.push(child),
                "namespace_use_group" => {
                    let mut group_cursor = child.walk();
                    clauses.extend(
                        child
                            .named_children(&mut group_cursor)
                            .filter(|c| c.kind() == "namespace_use_clause"),
                    );
                }
                _ => {}
            }
        }

        clauses
            .into_iter()
            .filter_map(|clause| {
                let mut leaf: Option<String> = None;
                let mut alias: Option<String> = None;
                let mut clause_cursor = clause.walk();
                for child in clause.named_children(&mut clause_cursor) {
                    if clause.child_by_field_name("alias") == Some(child) {
                        alias = text(child);
                    } else if leaf.is_none() && matches!(child.kind(), "name" | "qualified_name") {
                        leaf = text(child);
                    }
                }
                let leaf = leaf?;
                let path = match &prefix {
                    Some(prefix) => format!("{}\\{}", prefix.trim_end_matches('\\'), leaf),
                    None => leaf,
                };
                let name = alias.or_else(|| path.split('\\').next_back().map(String::from));
                // PHP has no standard library of namespaces: a namespace is
                // the project's own (#147) or a dependency's.
                let kind = if context.is_local_package(&path, Language::Php) {
                    ImportKind::Local
                } else {
                    ImportKind::External
                };
                Some(Import {
                    path,
                    name,
                    kind,
                    start_line,
                    end_line,
                })
            })
            .collect()
    }

    /// `require 'x'` and `require_relative 'x'`: a `call` without receiver
    /// whose `method` is one of the two (#84: `requirements.push("json")`
    /// also starts with `require`). A `require_relative` path is stored with
    /// a leading `./` (`./x`) so the resolver looks for it next to the file
    /// only, where `require 'x'` goes through the load path.
    fn extract_ruby_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "call" || node.child_by_field_name("receiver").is_some() {
            return None;
        }
        let method = node
            .child_by_field_name("method")?
            .utf8_text(source.as_bytes())
            .ok()?;
        if method != "require" && method != "require_relative" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the string argument
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "argument_list" {
                let mut inner_cursor = child.walk();
                for arg in child.children(&mut inner_cursor) {
                    if arg.kind() == "string" {
                        let path_text = arg.utf8_text(source.as_bytes()).ok()?;
                        let mut path = path_text
                            .trim_matches(|c| c == '"' || c == '\'')
                            .to_string();
                        if method == "require_relative" && !path.starts_with('.') {
                            path = format!("./{path}");
                        }

                        let kind = if method == "require_relative" || path.starts_with('.') {
                            ImportKind::Local
                        } else {
                            ImportKind::External
                        };

                        let name = path.split('/').next_back().map(String::from);

                        return Some(Import {
                            path,
                            name,
                            kind,
                            start_line,
                            end_line,
                        });
                    }
                }
            }
        }

        None
    }

    /// `using A.B;`, `using static A.B.C;` and `using X = A.B.C;` (the alias
    /// is the `name` field, the namespace the `qualified_name`).
    fn extract_csharp_import(node: &Node, source: &str, context: &ImportContext) -> Option<Import> {
        if node.kind() != "using_directive" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        let alias = node.child_by_field_name("name");
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if (child.kind() == "qualified_name" || child.kind() == "identifier")
                && Some(child) != alias
            {
                let path = child.utf8_text(source.as_bytes()).ok()?.to_string();

                let kind = if context.is_local_package(&path, Language::CSharp) {
                    ImportKind::Local
                } else if path == "System" || path.starts_with("System.") {
                    ImportKind::Std
                } else {
                    ImportKind::External
                };

                let name = path.split('.').next_back().map(String::from);

                return Some(Import {
                    path,
                    name,
                    kind,
                    start_line,
                    end_line,
                });
            }
        }

        None
    }

    /// `import a.b.C`, `import a.b.C as D`, `import a.b.*` (tree-sitter-kotlin-ng:
    /// an `import` node holding a `qualified_identifier`, then an optional alias
    /// `identifier`; the `.*` wildcard is an anonymous token).
    fn extract_kotlin_import(node: &Node, source: &str, context: &ImportContext) -> Option<Import> {
        if node.kind() != "import" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let src = source.as_bytes();

        let mut path: Option<String> = None;
        let mut alias: Option<String> = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "qualified_identifier" if path.is_none() => {
                    path = Some(child.utf8_text(src).ok()?.to_string());
                }
                "identifier" => alias = Some(child.utf8_text(src).ok()?.to_string()),
                _ => {}
            }
        }
        let path = path?;
        let wildcard = node.utf8_text(src).ok()?.trim_end().ends_with(".*");

        let kind = if context.is_local_package(&path, Language::Kotlin) {
            ImportKind::Local
        } else if ["kotlin.", "kotlinx.", "java.", "javax."]
            .iter()
            .any(|p| path.starts_with(p))
        {
            ImportKind::Std
        } else {
            ImportKind::External
        };

        let name = if wildcard {
            None
        } else {
            alias.or_else(|| path.split('.').next_back().map(String::from))
        };

        Some(Import {
            path,
            name,
            kind,
            start_line,
            end_line,
        })
    }

    /// `import a.b.C`, `import a.b._` and the selector form
    /// `import a.b.{C, D => E, _}`: one `Import` per leaf (alias as `name`,
    /// `None` for a wildcard), like the Rust brace expansion.
    fn extract_scala_import(node: &Node, source: &str, context: &ImportContext) -> Vec<Import> {
        if node.kind() != "import_declaration" {
            return Vec::new();
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let src = source.as_bytes();
        let text = |n: Node| n.utf8_text(src).ok().map(str::to_string);

        let mut cursor = node.walk();
        let prefix = node
            .children_by_field_name("path", &mut cursor)
            .filter(|n| n.is_named())
            .filter_map(text)
            .collect::<Vec<_>>()
            .join(".");

        // (path, name) leaves
        let mut leaves: Vec<(String, Option<String>)> = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "namespace_wildcard" => leaves.push((prefix.clone(), None)),
                "namespace_selectors" => {
                    let mut sel_cursor = child.walk();
                    for selector in child.named_children(&mut sel_cursor) {
                        let leaf = match selector.kind() {
                            "namespace_wildcard" => (prefix.clone(), None),
                            "arrow_renamed_identifier" => {
                                let Some(name) =
                                    selector.child_by_field_name("name").and_then(text)
                                else {
                                    continue;
                                };
                                let alias = selector.child_by_field_name("alias").and_then(text);
                                (format!("{prefix}.{name}"), alias.or(Some(name)))
                            }
                            _ => {
                                let Some(name) = text(selector) else {
                                    continue;
                                };
                                (format!("{prefix}.{name}"), Some(name))
                            }
                        };
                        leaves.push(leaf);
                    }
                }
                _ => {}
            }
        }
        if leaves.is_empty() && !prefix.is_empty() {
            let name = prefix.split('.').next_back().map(String::from);
            leaves.push((prefix.clone(), name));
        }

        leaves
            .into_iter()
            .map(|(path, name)| {
                let kind = if context.is_local_package(&path, Language::Scala) {
                    ImportKind::Local
                } else if path.starts_with("scala.") || path.starts_with("java.") {
                    ImportKind::Std
                } else {
                    ImportKind::External
                };
                Import {
                    path,
                    name,
                    kind,
                    start_line,
                    end_line,
                }
            })
            .collect()
    }

    fn extract_bash_import(node: &Node, source: &str) -> Option<Import> {
        // Bash uses source or . for imports
        if node.kind() != "command" {
            return None;
        }

        let text = node.utf8_text(source.as_bytes()).ok()?;
        if !text.starts_with("source ") && !text.starts_with(". ") {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        let path = text
            .strip_prefix("source ")
            .or_else(|| text.strip_prefix(". "))
            .unwrap_or(text)
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();

        let name = path.split('/').next_back().map(String::from);

        Some(Import {
            path,
            name,
            kind: ImportKind::Local,
            start_line,
            end_line,
        })
    }

    /// `import`, `alias`, `use` and `require` directives: a `call` whose
    /// `target` is the directive and whose first argument names the module
    /// (an `alias` node, an atom for Erlang modules). The brace form `alias
    /// A.{B, C}` (a `dot` of an alias and a tuple) gives one `Import` per
    /// leaf (#84); `as: X` is the binding name.
    fn extract_elixir_import(node: &Node, source: &str) -> Vec<Import> {
        if node.kind() != "call" {
            return Vec::new();
        }
        let src = source.as_bytes();
        let text = |n: Node| n.utf8_text(src).ok().map(str::to_string);
        let Some(target) = node.child_by_field_name("target").and_then(text) else {
            return Vec::new();
        };
        if !matches!(target.as_str(), "import" | "alias" | "use" | "require") {
            return Vec::new();
        }
        let mut cursor = node.walk();
        let Some(arguments) = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "arguments")
        else {
            return Vec::new();
        };
        let mut args_cursor = arguments.walk();
        let args: Vec<Node> = arguments.named_children(&mut args_cursor).collect();
        let Some(first) = args.first() else {
            return Vec::new();
        };

        let mut paths: Vec<String> = Vec::new();
        Self::elixir_module_paths(*first, "", src, &mut paths);
        if paths.is_empty() {
            return Vec::new();
        }
        // `alias Foo.Bar, as: Baz`
        let alias = args.iter().skip(1).find_map(|arg| {
            if arg.kind() != "keywords" {
                return None;
            }
            let mut pair_cursor = arg.walk();
            arg.named_children(&mut pair_cursor).find_map(|pair| {
                let key = pair.child_by_field_name("key").and_then(text)?;
                (key.trim().trim_end_matches(':') == "as")
                    .then(|| pair.child_by_field_name("value").and_then(text))
                    .flatten()
            })
        });

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let single = paths.len() == 1;
        paths
            .into_iter()
            .map(|path| {
                let kind = if path.starts_with("Elixir.") || path.starts_with(':') {
                    ImportKind::Std
                } else {
                    ImportKind::External
                };
                let name = if single && alias.is_some() {
                    alias.clone()
                } else {
                    path.split('.').next_back().map(String::from)
                };
                Import {
                    path,
                    name,
                    kind,
                    start_line,
                    end_line,
                }
            })
            .collect()
    }

    /// Module paths an Elixir directive argument names, under `prefix`:
    /// `A.B` for an alias or atom, one per leaf of `A.{B, C.{D, E}}`.
    fn elixir_module_paths(node: Node, prefix: &str, src: &[u8], out: &mut Vec<String>) {
        let join = |leaf: &str| {
            if prefix.is_empty() {
                leaf.to_string()
            } else {
                format!("{prefix}.{leaf}")
            }
        };
        match node.kind() {
            "alias" | "atom" => {
                if let Ok(text) = node.utf8_text(src) {
                    out.push(join(text.trim()));
                }
            }
            "dot" => {
                let Some(left) = node
                    .child_by_field_name("left")
                    .and_then(|l| l.utf8_text(src).ok())
                else {
                    return;
                };
                let prefix = join(left.trim());
                match node.child_by_field_name("right") {
                    Some(right) if right.kind() == "tuple" => {
                        let mut cursor = right.walk();
                        for leaf in right.named_children(&mut cursor) {
                            Self::elixir_module_paths(leaf, &prefix, src, out);
                        }
                    }
                    Some(right) => Self::elixir_module_paths(right, &prefix, src, out),
                    None => out.push(prefix),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::LanguageSupport;

    #[test]
    fn test_extract_rust_imports() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use std::collections::HashMap;
use anyhow::Result;
use crate::utils::helper;
pub use crate::config::Config;
pub(crate) use crate::store::IndexStore;
pub(super) use crate::utils::Helper;
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        // All 6 use_declarations must be picked up, regardless of visibility.
        // Before the fix, pub/pub(crate)/pub(super) variants were silently dropped
        // by `parse_rust_use_path`, which broke `deps` on every Rust `lib.rs`.
        assert_eq!(imports.len(), 6, "got: {:#?}", imports);

        assert_eq!(imports[0].path, "std::collections::HashMap");
        assert_eq!(imports[0].kind, ImportKind::Std);

        assert_eq!(imports[1].path, "anyhow::Result");
        assert_eq!(imports[1].kind, ImportKind::External);

        assert_eq!(imports[2].path, "crate::utils::helper");
        assert_eq!(imports[2].kind, ImportKind::Local);

        assert_eq!(imports[3].path, "crate::config::Config");
        assert_eq!(imports[3].kind, ImportKind::Local);

        assert_eq!(imports[4].path, "crate::store::IndexStore");
        assert_eq!(imports[4].kind, ImportKind::Local);

        assert_eq!(imports[5].path, "crate::utils::Helper");
        assert_eq!(imports[5].kind, ImportKind::Local);
    }

    #[test]
    fn test_extract_typescript_imports() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
import { useState } from 'react';
import axios from 'axios';
import { helper } from './utils';
"#;
        let tree = support.parse(Language::TypeScript, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::TypeScript).unwrap();

        assert_eq!(imports.len(), 3);

        assert_eq!(imports[0].path, "react");
        assert_eq!(imports[0].kind, ImportKind::External);

        assert_eq!(imports[1].path, "axios");
        assert_eq!(imports[1].kind, ImportKind::External);

        assert_eq!(imports[2].path, "./utils");
        assert_eq!(imports[2].kind, ImportKind::Local);
    }

    #[test]
    fn test_extract_python_imports() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
import os
import json
from collections import defaultdict
from .local_module import helper
"#;
        let tree = support.parse(Language::Python, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Python).unwrap();

        assert!(
            imports
                .iter()
                .any(|i| i.path == "os" && i.kind == ImportKind::Std)
        );
        assert!(
            imports
                .iter()
                .any(|i| i.path == "json" && i.kind == ImportKind::Std)
        );
        assert!(
            imports
                .iter()
                .any(|i| i.path == "collections" && i.kind == ImportKind::Std)
        );
    }

    #[test]
    fn test_extract_go_imports() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
package main

import (
    "fmt"
    "github.com/pkg/errors"
)
"#;
        let tree = support.parse(Language::Go, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Go).unwrap();

        assert!(
            imports
                .iter()
                .any(|i| i.path == "fmt" && i.kind == ImportKind::Std)
        );
        assert!(
            imports
                .iter()
                .any(|i| i.path == "github.com/pkg/errors" && i.kind == ImportKind::External)
        );
    }

    #[test]
    fn test_extract_java_imports() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
import java.util.List;
import java.util.ArrayList;
import com.google.gson.Gson;
"#;
        let tree = support.parse(Language::Java, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Java).unwrap();

        assert!(
            imports
                .iter()
                .any(|i| i.path.starts_with("java.util") && i.kind == ImportKind::Std)
        );
        assert!(
            imports
                .iter()
                .any(|i| i.path.starts_with("com.google") && i.kind == ImportKind::External)
        );
    }

    #[test]
    fn test_extract_c_imports() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
#include <stdio.h>
#include <stdlib.h>
#include "myheader.h"
"#;
        let tree = support.parse(Language::C, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::C).unwrap();

        assert!(
            imports
                .iter()
                .any(|i| i.path == "stdio.h" && i.kind == ImportKind::Std)
        );
        assert!(
            imports
                .iter()
                .any(|i| i.path == "stdlib.h" && i.kind == ImportKind::Std)
        );
        assert!(
            imports
                .iter()
                .any(|i| i.path == "myheader.h" && i.kind == ImportKind::Local)
        );
    }

    #[test]
    fn test_classify_python_stdlib_comprehensive() {
        // Standard library modules should be classified as Std
        let std_imports = [
            "hashlib",
            "math",
            "sqlite3",
            "http",
            "xml",
            "email",
            "os",
            "sys",
            "json",
            "collections",
            "itertools",
            "functools",
            "typing",
            "dataclasses",
            "abc",
            "io",
            "time",
            "datetime",
            "logging",
            "unittest",
            "argparse",
            "subprocess",
            "threading",
            "asyncio",
            "pathlib",
            "re",
            "uuid",
            "urllib",
            "csv",
            "struct",
            "socket",
            "ssl",
            "tempfile",
            "shutil",
            "glob",
            "pickle",
            "enum",
            "secrets",
            "statistics",
        ];
        for module in &std_imports {
            assert_eq!(
                ImportExtractor::classify_python_import(module),
                ImportKind::Std,
                "'{}' should be classified as Std",
                module
            );
        }

        // External packages should NOT be classified as Std
        let external_imports = ["numpy", "pandas", "requests", "flask", "django", "pytest"];
        for module in &external_imports {
            assert_eq!(
                ImportExtractor::classify_python_import(module),
                ImportKind::External,
                "'{}' should be classified as External",
                module
            );
        }

        // Dotted paths should classify by first segment
        assert_eq!(
            ImportExtractor::classify_python_import("os.path"),
            ImportKind::Std
        );
        assert_eq!(
            ImportExtractor::classify_python_import("http.server"),
            ImportKind::Std
        );
        assert_eq!(
            ImportExtractor::classify_python_import("numpy.linalg"),
            ImportKind::External
        );
    }

    #[test]
    fn test_import_kind_as_str() {
        assert_eq!(ImportKind::Std.as_str(), "std");
        assert_eq!(ImportKind::External.as_str(), "external");
        assert_eq!(ImportKind::Local.as_str(), "local");
    }

    #[test]
    fn test_import_kind_display_and_from_str_roundtrip() {
        use std::str::FromStr;
        for k in [ImportKind::Std, ImportKind::External, ImportKind::Local] {
            assert_eq!(k.to_string(), k.as_str());
            assert_eq!(ImportKind::from_str(k.as_str()), Ok(k));
        }
        assert_eq!(ImportKind::from_str("STD"), Ok(ImportKind::Std));
        assert!(ImportKind::from_str("unknown").is_err());
    }

    #[test]
    fn test_rust_import_with_braces() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use std::collections::{HashMap, HashSet};
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        // Brace imports are now expanded into one Import per leaf.
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].path, "std::collections::HashMap");
        assert_eq!(imports[0].name.as_deref(), Some("HashMap"));
        assert_eq!(imports[0].kind, ImportKind::Std);
        assert_eq!(imports[1].path, "std::collections::HashSet");
        assert_eq!(imports[1].name.as_deref(), Some("HashSet"));
        assert_eq!(imports[1].kind, ImportKind::Std);
    }

    #[test]
    fn test_rust_brace_import_std_io() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use std::io::{Read, Write};
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].path, "std::io::Read");
        assert_eq!(imports[0].name.as_deref(), Some("Read"));
        assert_eq!(imports[0].kind, ImportKind::Std);
        assert_eq!(imports[1].path, "std::io::Write");
        assert_eq!(imports[1].name.as_deref(), Some("Write"));
        assert_eq!(imports[1].kind, ImportKind::Std);
    }

    #[test]
    fn test_rust_brace_import_with_alias() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use foo::{A as X, B};
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].path, "foo::A");
        // Alias takes precedence over the original symbol name.
        assert_eq!(imports[0].name.as_deref(), Some("X"));
        assert_eq!(imports[1].path, "foo::B");
        assert_eq!(imports[1].name.as_deref(), Some("B"));
    }

    #[test]
    fn test_rust_brace_import_with_nested_prefix() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use foo::nested::{A, B};
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].path, "foo::nested::A");
        assert_eq!(imports[0].name.as_deref(), Some("A"));
        assert_eq!(imports[1].path, "foo::nested::B");
        assert_eq!(imports[1].name.as_deref(), Some("B"));
    }

    #[test]
    fn test_rust_glob_import() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use foo::*;
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 1);
        // Glob: keep at least the prefix, name is None.
        assert_eq!(imports[0].path, "foo");
        assert!(imports[0].name.is_none());
    }

    #[test]
    fn test_rust_brace_import_nested_nested() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use foo::{a::{X, Y}, B};
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].path, "foo::a::X");
        assert_eq!(imports[0].name.as_deref(), Some("X"));
        assert_eq!(imports[1].path, "foo::a::Y");
        assert_eq!(imports[1].name.as_deref(), Some("Y"));
        assert_eq!(imports[2].path, "foo::B");
        assert_eq!(imports[2].name.as_deref(), Some("B"));
    }

    #[test]
    fn test_rust_super_import() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use super::parent_module;
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::Local);
    }

    fn imports(lang: Language, source: &str) -> Vec<(String, Option<String>, ImportKind)> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        ImportExtractor::extract(&tree, source, lang)
            .unwrap()
            .into_iter()
            .map(|i| (i.path, i.name, i.kind))
            .collect()
    }

    fn import(
        path: &str,
        name: Option<&str>,
        kind: ImportKind,
    ) -> (String, Option<String>, ImportKind) {
        (path.to_string(), name.map(String::from), kind)
    }

    #[test]
    fn test_extract_kotlin_imports() {
        // tree-sitter-kotlin-ng emits `import`, not the old `import_header`.
        let source = "package a.b\nimport foo.bar.Baz\nimport kotlin.collections.List as KList\nimport foo.*\nimport javax.inject.Inject\n\nclass X {\n    fun f() { Baz() }\n}\n";
        assert_eq!(
            imports(Language::Kotlin, source),
            vec![
                import("foo.bar.Baz", Some("Baz"), ImportKind::External),
                import("kotlin.collections.List", Some("KList"), ImportKind::Std),
                import("foo", None, ImportKind::External),
                import("javax.inject.Inject", Some("Inject"), ImportKind::Std),
            ]
        );
    }

    #[test]
    fn test_python_import_alias_and_list() {
        let source = "import numpy as np\nimport os, sys\nimport os.path as osp\nfrom . import sibling\nfrom .. import a, b\nfrom .mod import x as y\nfrom collections import defaultdict\nfrom . import *\nfrom .pkg import *\n";
        assert_eq!(
            imports(Language::Python, source),
            vec![
                import("numpy", Some("np"), ImportKind::External),
                import("os", Some("os"), ImportKind::Std),
                import("sys", Some("sys"), ImportKind::Std),
                import("os.path", Some("osp"), ImportKind::Std),
                import(".", Some("sibling"), ImportKind::Local),
                import("..", Some("a"), ImportKind::Local),
                import("..", Some("b"), ImportKind::Local),
                import(".mod", Some("mod"), ImportKind::Local),
                import("collections", Some("collections"), ImportKind::Std),
                import(".", None, ImportKind::Local),
                import(".pkg", Some("pkg"), ImportKind::Local),
            ]
        );
    }

    #[test]
    fn test_ts_reexports_and_require_are_imports() {
        let source = "export { greet, Foo as Bar } from './foo';\nexport * from './bar';\nexport * as ns from './ns';\nexport { qux };\nexport type { T } from './t';\nconst { helper } = require('./util');\nconst fs = require('fs');\nconst x = require(name);\nexport function f() {}\n";
        assert_eq!(
            imports(Language::TypeScript, source),
            vec![
                import("./foo", Some("foo"), ImportKind::Local),
                import("./bar", Some("bar"), ImportKind::Local),
                import("./ns", Some("ns"), ImportKind::Local),
                import("./t", Some("t"), ImportKind::Local),
                import("./util", Some("util"), ImportKind::Local),
                import("fs", Some("fs"), ImportKind::External),
            ]
        );
        let js = "const { helper } = require('./util');\nmodule.exports = require('./x');\nexport { a } from './a';\n";
        assert_eq!(
            imports(Language::JavaScript, js),
            vec![
                import("./util", Some("util"), ImportKind::Local),
                import("./x", Some("x"), ImportKind::Local),
                import("./a", Some("a"), ImportKind::Local),
            ]
        );
    }

    #[test]
    fn test_scala_selector_imports() {
        let source = "import a.b.{C, D => E, _}\nimport scala.collection.mutable\nimport x.y._\n";
        assert_eq!(
            imports(Language::Scala, source),
            vec![
                import("a.b.C", Some("C"), ImportKind::External),
                import("a.b.D", Some("E"), ImportKind::External),
                import("a.b", None, ImportKind::External),
                import("scala.collection.mutable", Some("mutable"), ImportKind::Std),
                import("x.y", None, ImportKind::External),
            ]
        );
    }

    #[test]
    fn test_php_group_and_list_imports() {
        let source = "<?php\nuse Foo\\{A, B as C};\nuse Foo\\Bar;\nuse function Foo\\baz;\nuse X\\Y, Z\\W as V;\n";
        assert_eq!(
            imports(Language::Php, source),
            vec![
                import("Foo\\A", Some("A"), ImportKind::External),
                import("Foo\\B", Some("C"), ImportKind::External),
                import("Foo\\Bar", Some("Bar"), ImportKind::External),
                import("Foo\\baz", Some("baz"), ImportKind::External),
                import("X\\Y", Some("Y"), ImportKind::External),
                import("Z\\W", Some("V"), ImportKind::External),
            ]
        );
    }

    #[test]
    fn test_rust_use_group_comments_are_dropped() {
        let source = "use foo::{ // keep\n    Bar, /* c */ Baz,\n};\nuse std::io; // trailing\n";
        assert_eq!(
            imports(Language::Rust, source),
            vec![
                import("foo::Bar", Some("Bar"), ImportKind::External),
                import("foo::Baz", Some("Baz"), ImportKind::External),
                import("std::io", Some("io"), ImportKind::Std),
            ]
        );
    }

    #[test]
    fn test_go_module_local_imports() {
        let source = "package main\nimport (\n    \"fmt\"\n    \"myapp/internal/store\"\n    \"myapp\"\n    \"myapplication/x\"\n    s \"github.com/x/y\"\n)\n";
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(Language::Go, source).unwrap();
        let kinds = |context: &ImportContext| {
            ImportExtractor::extract_with_context(&tree, source, Language::Go, context)
                .unwrap()
                .into_iter()
                .map(|i| (i.path, i.kind))
                .collect::<Vec<_>>()
        };
        let context = ImportContext {
            go_module: Some("myapp".into()),
            ..ImportContext::default()
        };
        assert_eq!(
            kinds(&context),
            vec![
                ("fmt".to_string(), ImportKind::Std),
                ("myapp/internal/store".to_string(), ImportKind::Local),
                ("myapp".to_string(), ImportKind::Local),
                // Not under the module: the old first-segment heuristic.
                ("myapplication/x".to_string(), ImportKind::Std),
                ("github.com/x/y".to_string(), ImportKind::External),
            ]
        );
        assert_eq!(kinds(&ImportContext::default())[1].1, ImportKind::Std);
    }

    #[test]
    fn test_go_module_path_from_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(go_module_path(dir.path()), None);
        std::fs::write(
            dir.path().join("go.mod"),
            "// a comment\nmodule example.com/myapp // v2\n\ngo 1.22\n",
        )
        .unwrap();
        assert_eq!(
            go_module_path(dir.path()).as_deref(),
            Some("example.com/myapp")
        );
        assert_eq!(
            ImportContext::from_project_root(dir.path())
                .go_module
                .as_deref(),
            Some("example.com/myapp")
        );
    }

    #[test]
    fn test_import_line_numbers() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
use std::io;

fn main() {}

use std::fs;
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].start_line, 2);
        assert_eq!(imports[1].start_line, 6);
    }
}
