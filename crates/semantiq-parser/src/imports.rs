use crate::language::Language;
use crate::python_stdlib::PYTHON_STD_MODULES;
use anyhow::Result;
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

pub struct ImportExtractor;

impl ImportExtractor {
    pub fn extract(tree: &Tree, source: &str, language: Language) -> Result<Vec<Import>> {
        let mut imports = Vec::new();
        let root = tree.root_node();

        Self::extract_recursive(&root, source, language, &mut imports)?;

        Ok(imports)
    }

    fn extract_recursive(
        node: &Node,
        source: &str,
        language: Language,
        imports: &mut Vec<Import>,
    ) -> Result<()> {
        for import in Self::node_to_import(node, source, language) {
            imports.push(import);
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::extract_recursive(&child, source, language, imports)?;
        }

        Ok(())
    }

    fn node_to_import(node: &Node, source: &str, language: Language) -> Vec<Import> {
        match language {
            Language::Rust => Self::extract_rust_import(node, source),
            Language::TypeScript | Language::JavaScript => {
                Self::extract_ts_import(node, source).into_iter().collect()
            }
            Language::Python => Self::extract_python_import(node, source).into_iter().collect(),
            Language::Go => Self::extract_go_import(node, source).into_iter().collect(),
            Language::Java => Self::extract_java_import(node, source).into_iter().collect(),
            Language::C | Language::Cpp => {
                Self::extract_c_import(node, source).into_iter().collect()
            }
            Language::Php => Self::extract_php_import(node, source).into_iter().collect(),
            Language::Ruby => Self::extract_ruby_import(node, source).into_iter().collect(),
            Language::CSharp => Self::extract_csharp_import(node, source).into_iter().collect(),
            Language::Kotlin => Self::extract_kotlin_import(node, source).into_iter().collect(),
            Language::Scala => Self::extract_scala_import(node, source).into_iter().collect(),
            // Markup/config languages don't have traditional imports
            Language::Html | Language::Json | Language::Yaml | Language::Toml => Vec::new(),
            Language::Bash => Self::extract_bash_import(node, source).into_iter().collect(),
            Language::Elixir => Self::extract_elixir_import(node, source).into_iter().collect(),
        }
    }

    fn extract_rust_import(node: &Node, source: &str) -> Vec<Import> {
        if node.kind() != "use_declaration" {
            return Vec::new();
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Get the full use path text
        let text = &source[node.start_byte()..node.end_byte()];

        // Extract the path body from "use path::to::module;" / "pub use ...;"
        let body = match Self::parse_rust_use_path(text) {
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

    fn parse_rust_use_path(text: &str) -> Option<String> {
        // Remove "use " prefix and ";" suffix
        let text = text.trim();
        let text = text.strip_prefix("use ")?.strip_suffix(';')?.trim();

        // Handle "pub use" case
        let text = text.strip_prefix("pub ").unwrap_or(text);
        let text = text.strip_prefix("use ").unwrap_or(text);

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
            (body[..idx].trim().to_string(), Some(body[idx + 4..].trim().to_string()))
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
                let name = alias.or_else(|| {
                    prefix.rsplit("::").next().map(String::from)
                });
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

    fn extract_ts_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "import_statement" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the source (string) child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "string" {
                let path_text = &source[child.start_byte()..child.end_byte()];
                let path = path_text
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string();

                let kind = if path.starts_with('.') {
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

        None
    }

    fn extract_python_import(node: &Node, source: &str) -> Option<Import> {
        match node.kind() {
            "import_statement" => {
                let start_line = node.start_position().row + 1;
                let end_line = node.end_position().row + 1;

                // Find the dotted_name child
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "dotted_name" {
                        let path = source[child.start_byte()..child.end_byte()].to_string();
                        let kind = Self::classify_python_import(&path);
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
            "import_from_statement" => {
                let start_line = node.start_position().row + 1;
                let end_line = node.end_position().row + 1;

                // Find the module_name child
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "dotted_name" || child.kind() == "relative_import" {
                        let path = source[child.start_byte()..child.end_byte()].to_string();
                        let kind = if path.starts_with('.') {
                            ImportKind::Local
                        } else {
                            Self::classify_python_import(&path)
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
            _ => None,
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

    fn extract_go_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "import_spec" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the interpreted_string_literal child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "interpreted_string_literal" {
                let path_text = &source[child.start_byte()..child.end_byte()];
                let path = path_text.trim_matches('"').to_string();

                let kind = if path.starts_with('.') || path.starts_with('/') {
                    ImportKind::Local
                } else if path.contains('.') {
                    // External packages usually have dots (e.g., github.com/...)
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
                let path = source[child.start_byte()..child.end_byte()].to_string();

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
                    let path_text = &source[child.start_byte()..child.end_byte()];
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
                    let path_text = &source[child.start_byte()..child.end_byte()];
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

    fn extract_php_import(node: &Node, source: &str) -> Option<Import> {
        // Handle "use" statements (namespace imports)
        if node.kind() != "namespace_use_declaration" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Get the full text of the use statement
        let text = &source[node.start_byte()..node.end_byte()];

        // Parse "use Namespace\Class;" or "use Namespace\Class as Alias;"
        let path = Self::parse_php_use_path(text)?;
        let name = path.split('\\').next_back().map(String::from);

        // PHP doesn't have a standard library in the same sense, most are external
        let kind = ImportKind::External;

        Some(Import {
            path,
            name,
            kind,
            start_line,
            end_line,
        })
    }

    fn parse_php_use_path(text: &str) -> Option<String> {
        let text = text.trim();
        // Remove "use " prefix and ";" suffix
        let text = text.strip_prefix("use ")?.trim();
        let text = text.strip_suffix(';').unwrap_or(text).trim();

        // Handle "as Alias" clause
        let path = if let Some(idx) = text.find(" as ") {
            &text[..idx]
        } else {
            text
        };

        Some(path.trim().to_string())
    }

    fn extract_ruby_import(node: &Node, source: &str) -> Option<Import> {
        // Ruby uses require and require_relative
        if node.kind() != "call" {
            return None;
        }

        let text = &source[node.start_byte()..node.end_byte()];
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
                        let path_text = &source[arg.start_byte()..arg.end_byte()];
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
                let path = source[child.start_byte()..child.end_byte()].to_string();

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

    fn extract_kotlin_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "import_header" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Find the identifier child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "identifier" {
                let path = source[child.start_byte()..child.end_byte()].to_string();

                let kind = if path.starts_with("kotlin.") || path.starts_with("java.") {
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

    fn extract_scala_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "import_declaration" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        let text = &source[node.start_byte()..node.end_byte()];
        let path = text
            .strip_prefix("import ")
            .unwrap_or(text)
            .trim()
            .to_string();

        let kind = if path.starts_with("scala.") || path.starts_with("java.") {
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

    fn extract_bash_import(node: &Node, source: &str) -> Option<Import> {
        // Bash uses source or . for imports
        if node.kind() != "command" {
            return None;
        }

        let text = &source[node.start_byte()..node.end_byte()];
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

        let text = &source[node.start_byte()..node.end_byte()];
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
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let imports = ImportExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert_eq!(imports.len(), 3);

        assert_eq!(imports[0].path, "std::collections::HashMap");
        assert_eq!(imports[0].kind, ImportKind::Std);

        assert_eq!(imports[1].path, "anyhow::Result");
        assert_eq!(imports[1].kind, ImportKind::External);

        assert_eq!(imports[2].path, "crate::utils::helper");
        assert_eq!(imports[2].kind, ImportKind::Local);
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
