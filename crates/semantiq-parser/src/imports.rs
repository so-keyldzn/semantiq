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
        if let Some(import) = Self::node_to_import(node, source, language) {
            imports.push(import);
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::extract_recursive(&child, source, language, imports)?;
        }

        Ok(())
    }

    fn node_to_import(node: &Node, source: &str, language: Language) -> Option<Import> {
        match language {
            Language::Rust => Self::extract_rust_import(node, source),
            Language::TypeScript | Language::JavaScript => Self::extract_ts_import(node, source),
            Language::Python => Self::extract_python_import(node, source),
            Language::Go => Self::extract_go_import(node, source),
            Language::Java => Self::extract_java_import(node, source),
            Language::C | Language::Cpp => Self::extract_c_import(node, source),
            Language::Php => Self::extract_php_import(node, source),
            Language::Ruby => Self::extract_ruby_import(node, source),
            Language::CSharp => Self::extract_csharp_import(node, source),
            Language::Kotlin => Self::extract_kotlin_import(node, source),
            Language::Scala => Self::extract_scala_import(node, source),
            // Markup/config languages don't have traditional imports
            Language::Html | Language::Json | Language::Yaml | Language::Toml => None,
            Language::Bash => Self::extract_bash_import(node, source),
            Language::Elixir => Self::extract_elixir_import(node, source),
        }
    }

    fn extract_rust_import(node: &Node, source: &str) -> Option<Import> {
        if node.kind() != "use_declaration" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Get the full use path
        let text = node.utf8_text(source.as_bytes()).ok()?;

        // Extract the path from "use path::to::module;"
        let path = Self::parse_rust_use_path(text)?;
        let kind = Self::classify_rust_import(&path);
        let name = Self::extract_rust_import_name(&path);

        Some(Import {
            path,
            name,
            kind,
            start_line,
            end_line,
        })
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

    fn classify_rust_import(path: &str) -> ImportKind {
        let first_segment = path.split("::").next().unwrap_or(path);

        match first_segment {
            "std" | "core" | "alloc" => ImportKind::Std,
            "crate" | "self" | "super" => ImportKind::Local,
            _ => ImportKind::External,
        }
    }

    fn extract_rust_import_name(path: &str) -> Option<String> {
        // Get the last segment of the path
        // Handle cases like "use foo::bar::{A, B}" -> return None
        if path.contains('{') {
            return None;
        }

        path.rsplit("::").next().map(String::from)
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
                let path_text = child.utf8_text(source.as_bytes()).ok()?;
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
                        let path = child.utf8_text(source.as_bytes()).ok()?.to_string();
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
                        let path = child.utf8_text(source.as_bytes()).ok()?.to_string();
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
                let path_text = child.utf8_text(source.as_bytes()).ok()?;
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

    fn extract_php_import(node: &Node, source: &str) -> Option<Import> {
        // Handle "use" statements (namespace imports)
        if node.kind() != "namespace_use_declaration" {
            return None;
        }

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        // Get the full text of the use statement
        let text = node.utf8_text(source.as_bytes()).ok()?;

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
                let path = child.utf8_text(source.as_bytes()).ok()?.to_string();

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

        let text = node.utf8_text(source.as_bytes()).ok()?;
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

        assert_eq!(imports.len(), 1);
        // Import with braces should have no specific name
        assert!(imports[0].name.is_none());
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
