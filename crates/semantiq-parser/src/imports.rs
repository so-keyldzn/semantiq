use crate::language::Language;
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
        let text = &source[node.start_byte()..node.end_byte()];

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
        // Remove "use " prefix and ";" suffix
        let text = text.trim();
        let text = text.strip_prefix("use ")?.strip_suffix(';')?.trim();

        // Handle "pub use" case
        let text = text.strip_prefix("pub ").unwrap_or(text);
        let text = text.strip_prefix("use ").unwrap_or(text);

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

        // Common Python standard library modules
        let std_modules = [
            "os",
            "sys",
            "re",
            "json",
            "pathlib",
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
        ];

        if std_modules.contains(&first_segment) {
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
