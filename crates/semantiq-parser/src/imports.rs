use crate::language::Language;
use crate::python_stdlib::PYTHON_STD_MODULES;
use anyhow::Result;
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

/// Project-level facts that change how imports are classified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportContext {
    /// `module` path of the project's `go.mod`: imports under it are `Local`
    /// (without it, `myapp/internal/store` looks like a standard package).
    pub go_module: Option<String>,
}

impl ImportContext {
    /// Reads the facts from the files at the project root (`go.mod`).
    pub fn from_project_root(project_root: &Path) -> Self {
        Self {
            go_module: go_module_path(project_root),
        }
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
            Language::Java => Self::extract_java_import(node, source)
                .into_iter()
                .collect(),
            Language::C | Language::Cpp => {
                Self::extract_c_import(node, source).into_iter().collect()
            }
            Language::Php => Self::extract_php_import(node, source),
            Language::Ruby => Self::extract_ruby_import(node, source)
                .into_iter()
                .collect(),
            Language::CSharp => Self::extract_csharp_import(node, source)
                .into_iter()
                .collect(),
            Language::Kotlin => Self::extract_kotlin_import(node, source)
                .into_iter()
                .collect(),
            Language::Scala => Self::extract_scala_import(node, source),
            // Markup/config languages don't have traditional imports
            Language::Html | Language::Json | Language::Yaml | Language::Toml => Vec::new(),
            Language::Bash => Self::extract_bash_import(node, source)
                .into_iter()
                .collect(),
            Language::Elixir => Self::extract_elixir_import(node, source)
                .into_iter()
                .collect(),
        }
    }

    fn extract_rust_import(node: &Node, source: &str) -> Vec<Import> {
        if node.kind() != "use_declaration" {
            return Vec::new();
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

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
                let kind = if in_module || path.starts_with('.') || path.starts_with('/') {
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

    fn extract_java_import(node: &Node, source: &str) -> Option<Import> {
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

                let kind = if path.starts_with("java.") || path.starts_with("javax.") {
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
    fn extract_php_import(node: &Node, source: &str) -> Vec<Import> {
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
                Some(Import {
                    path,
                    name,
                    // PHP doesn't have a standard library in the same sense, most are external
                    kind: ImportKind::External,
                    start_line,
                    end_line,
                })
            })
            .collect()
    }

    fn extract_ruby_import(node: &Node, source: &str) -> Option<Import> {
        // Ruby uses require and require_relative
        if node.kind() != "call" {
            return None;
        }

        let text = node.utf8_text(source.as_bytes()).ok()?;
        if !text.starts_with("require") {
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
                        let path = path_text
                            .trim_matches(|c| c == '"' || c == '\'')
                            .to_string();

                        let kind = if text.starts_with("require_relative") {
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

    fn extract_csharp_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "using_directive" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the qualified_name child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "qualified_name" || child.kind() == "identifier" {
                let path = child.utf8_text(source.as_bytes()).ok()?.to_string();

                let kind = if path.starts_with("System") {
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
    fn extract_kotlin_import(node: &Node, source: &str) -> Option<Import> {
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

        let kind = if ["kotlin.", "kotlinx.", "java.", "javax."]
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
    fn extract_scala_import(node: &Node, source: &str) -> Vec<Import> {
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
                let kind = if path.starts_with("scala.") || path.starts_with("java.") {
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

    fn extract_elixir_import(node: &Node, source: &str) -> Option<Import> {
        // Elixir uses import, alias, use, require
        if node.kind() != "call" {
            return None;
        }

        let text = node.utf8_text(source.as_bytes()).ok()?;
        let is_import = text.starts_with("import ")
            || text.starts_with("alias ")
            || text.starts_with("use ")
            || text.starts_with("require ");

        if !is_import {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        let path = text
            .split_whitespace()
            .nth(1)
            .map(|s| s.trim_end_matches(','))
            .unwrap_or("")
            .to_string();

        if path.is_empty() {
            return None;
        }

        let kind = if path.starts_with("Elixir.") || path.starts_with(':') {
            ImportKind::Std
        } else {
            ImportKind::External
        };

        let name = path.split('.').next_back().map(String::from);

        Some(Import {
            path,
            name,
            kind,
            start_line,
            end_line,
        })
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
