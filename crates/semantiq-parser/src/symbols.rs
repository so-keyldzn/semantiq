use crate::language::Language;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Module,
    Variable,
    Constant,
    Type,
    Import,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Module => "module",
            SymbolKind::Variable => "variable",
            SymbolKind::Constant => "constant",
            SymbolKind::Type => "type",
            SymbolKind::Import => "import",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub parent: Option<String>,
}

pub struct SymbolExtractor;

impl SymbolExtractor {
    pub fn extract(tree: &Tree, source: &str, language: Language) -> Result<Vec<Symbol>> {
        let mut symbols = Vec::new();
        let root = tree.root_node();

        Self::extract_recursive(&root, source, language, &mut symbols, None)?;

        Ok(symbols)
    }

    fn extract_recursive(
        node: &Node,
        source: &str,
        language: Language,
        symbols: &mut Vec<Symbol>,
        parent: Option<&str>,
    ) -> Result<()> {
        if let Some(symbol) = Self::node_to_symbol(node, source, language, parent) {
            let parent_name = symbol.name.clone();
            symbols.push(symbol);

            // Extract children with this as parent
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                Self::extract_recursive(&child, source, language, symbols, Some(&parent_name))?;
            }
        } else {
            // Continue traversing
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                Self::extract_recursive(&child, source, language, symbols, parent)?;
            }
        }

        Ok(())
    }

    fn node_to_symbol(node: &Node, source: &str, language: Language, parent: Option<&str>) -> Option<Symbol> {
        let kind = Self::get_symbol_kind(node.kind(), language)?;
        let name = Self::extract_name(node, source, language)?;

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let start_byte = node.start_byte();
        let end_byte = node.end_byte();

        let signature = Self::extract_signature(node, source, language);
        let doc_comment = Self::extract_doc_comment(node, source);

        Some(Symbol {
            name,
            kind,
            start_line,
            end_line,
            start_byte,
            end_byte,
            signature,
            doc_comment,
            parent: parent.map(String::from),
        })
    }

    fn get_symbol_kind(node_kind: &str, language: Language) -> Option<SymbolKind> {
        match language {
            Language::Rust => Self::rust_symbol_kind(node_kind),
            Language::TypeScript | Language::JavaScript => Self::ts_symbol_kind(node_kind),
            Language::Python => Self::python_symbol_kind(node_kind),
            Language::Go => Self::go_symbol_kind(node_kind),
            Language::Java => Self::java_symbol_kind(node_kind),
            Language::C | Language::Cpp => Self::c_symbol_kind(node_kind),
            Language::Php => Self::php_symbol_kind(node_kind),
        }
    }

    fn rust_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_item" => Some(SymbolKind::Function),
            "struct_item" => Some(SymbolKind::Struct),
            "enum_item" => Some(SymbolKind::Enum),
            "trait_item" => Some(SymbolKind::Trait),
            "impl_item" => Some(SymbolKind::Class),
            "mod_item" => Some(SymbolKind::Module),
            "const_item" => Some(SymbolKind::Constant),
            "static_item" => Some(SymbolKind::Constant),
            "type_item" => Some(SymbolKind::Type),
            "use_declaration" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn ts_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_declaration" | "arrow_function" => Some(SymbolKind::Function),
            "method_definition" => Some(SymbolKind::Method),
            "class_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            "type_alias_declaration" => Some(SymbolKind::Type),
            "import_statement" => Some(SymbolKind::Import),
            "variable_declaration" => Some(SymbolKind::Variable),
            _ => None,
        }
    }

    fn python_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Class),
            "import_statement" | "import_from_statement" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn go_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_declaration" => Some(SymbolKind::Function),
            "method_declaration" => Some(SymbolKind::Method),
            "type_declaration" => Some(SymbolKind::Type),
            "struct_type" => Some(SymbolKind::Struct),
            "interface_type" => Some(SymbolKind::Interface),
            "const_declaration" => Some(SymbolKind::Constant),
            "var_declaration" => Some(SymbolKind::Variable),
            "import_declaration" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn java_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "method_declaration" => Some(SymbolKind::Method),
            "class_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            "import_declaration" => Some(SymbolKind::Import),
            "field_declaration" => Some(SymbolKind::Variable),
            _ => None,
        }
    }

    fn c_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            "struct_specifier" => Some(SymbolKind::Struct),
            "enum_specifier" => Some(SymbolKind::Enum),
            "type_definition" => Some(SymbolKind::Type),
            "preproc_include" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn php_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            "method_declaration" => Some(SymbolKind::Method),
            "class_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            "trait_declaration" => Some(SymbolKind::Trait),
            "enum_declaration" => Some(SymbolKind::Enum),
            "namespace_definition" => Some(SymbolKind::Module),
            "const_declaration" => Some(SymbolKind::Constant),
            "namespace_use_declaration" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn extract_name(node: &Node, source: &str, language: Language) -> Option<String> {
        // Try to find the identifier child
        let name_field = match language {
            Language::Rust => "name",
            Language::TypeScript | Language::JavaScript => "name",
            Language::Python => "name",
            Language::Go => "name",
            Language::Java => "name",
            Language::C | Language::Cpp => "declarator",
            Language::Php => "name",
        };

        if let Some(name_node) = node.child_by_field_name(name_field) {
            return Some(source[name_node.start_byte()..name_node.end_byte()].to_string());
        }

        // Fallback: look for identifier child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "identifier" || child.kind() == "type_identifier" {
                return Some(source[child.start_byte()..child.end_byte()].to_string());
            }
        }

        None
    }

    fn extract_signature(node: &Node, source: &str, _language: Language) -> Option<String> {
        // Get the first line of the node as a simple signature
        let text = &source[node.start_byte()..node.end_byte()];
        let first_line = text.lines().next()?;

        // Truncate if too long
        let sig = if first_line.len() > 200 {
            format!("{}...", &first_line[..200])
        } else {
            first_line.to_string()
        };

        Some(sig.trim().to_string())
    }

    fn extract_doc_comment(node: &Node, source: &str) -> Option<String> {
        // Look for preceding comment siblings
        let mut prev = node.prev_sibling();
        let mut comments = Vec::new();

        while let Some(sibling) = prev {
            if sibling.kind().contains("comment") {
                let comment = &source[sibling.start_byte()..sibling.end_byte()];
                comments.push(comment.to_string());
                prev = sibling.prev_sibling();
            } else {
                break;
            }
        }

        if comments.is_empty() {
            None
        } else {
            comments.reverse();
            Some(comments.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::LanguageSupport;

    #[test]
    fn test_extract_rust_symbols() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
/// A greeting function
fn hello(name: &str) -> String {
    format!("Hello, {}!", name)
}

struct User {
    name: String,
    age: u32,
}

impl User {
    fn new(name: String) -> Self {
        Self { name, age: 0 }
    }
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::Rust).unwrap();

        assert!(symbols.iter().any(|s| s.name == "hello" && s.kind == SymbolKind::Function));
        assert!(symbols.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Struct));
    }
}
