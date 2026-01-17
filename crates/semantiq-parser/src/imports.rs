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

        path.split("::").last().map(String::from)
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
                let path = path_text.trim_matches(|c| c == '"' || c == '\'').to_string();

                let kind = if path.starts_with('.') {
                    ImportKind::Local
                } else if path.starts_with('@') || !path.contains('/') {
                    ImportKind::External
                } else {
                    ImportKind::External
                };

                let name = path.split('/').last().map(String::from);

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
                        let name = path.split('.').last().map(String::from);

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
                        let name = path.split('.').last().map(String::from);

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
            "os", "sys", "re", "json", "pathlib", "collections", "itertools",
            "functools", "typing", "dataclasses", "abc", "io", "time", "datetime",
            "logging", "unittest", "argparse", "subprocess", "threading", "asyncio",
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

                let name = path.split('/').last().map(String::from);

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

                let name = path.split('.').last().map(String::from);

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
                    let name = path.split('/').last().map(String::from);

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
                    let name = path.split('/').last().map(String::from);

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
}
