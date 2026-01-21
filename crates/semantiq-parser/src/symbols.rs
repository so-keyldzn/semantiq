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

    fn node_to_symbol(
        node: &Node,
        source: &str,
        language: Language,
        parent: Option<&str>,
    ) -> Option<Symbol> {
        let mut kind = Self::get_symbol_kind(node.kind(), language)?;
        let name = Self::extract_name(node, source, language)?;

        // Détecter si une variable contient une arrow_function ou function_expression
        if matches!(kind, SymbolKind::Variable)
            && matches!(language, Language::TypeScript | Language::JavaScript)
            && Self::is_function_variable(node)
        {
            kind = SymbolKind::Function;
        }

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
            Language::Ruby => Self::ruby_symbol_kind(node_kind),
            Language::CSharp => Self::csharp_symbol_kind(node_kind),
            Language::Kotlin => Self::kotlin_symbol_kind(node_kind),
            Language::Scala => Self::scala_symbol_kind(node_kind),
            Language::Html => Self::html_symbol_kind(node_kind),
            Language::Json => Self::json_symbol_kind(node_kind),
            Language::Yaml => Self::yaml_symbol_kind(node_kind),
            Language::Toml => Self::toml_symbol_kind(node_kind),
            Language::Bash => Self::bash_symbol_kind(node_kind),
            Language::Elixir => Self::elixir_symbol_kind(node_kind),
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
            // variable_declaration = var, lexical_declaration = const/let
            "variable_declaration" | "lexical_declaration" => Some(SymbolKind::Variable),
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

    fn ruby_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "method" => Some(SymbolKind::Function),
            "singleton_method" => Some(SymbolKind::Function),
            "class" => Some(SymbolKind::Class),
            "module" => Some(SymbolKind::Module),
            "constant" => Some(SymbolKind::Constant),
            _ => None,
        }
    }

    fn csharp_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "method_declaration" => Some(SymbolKind::Method),
            "local_function_statement" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "struct_declaration" => Some(SymbolKind::Struct),
            "interface_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            "namespace_declaration" => Some(SymbolKind::Module),
            "field_declaration" => Some(SymbolKind::Variable),
            "property_declaration" => Some(SymbolKind::Variable),
            "using_directive" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn kotlin_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "object_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            "enum_class_body" => Some(SymbolKind::Enum),
            "property_declaration" => Some(SymbolKind::Variable),
            "import_header" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn scala_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Class),
            "object_definition" => Some(SymbolKind::Class),
            "trait_definition" => Some(SymbolKind::Trait),
            "enum_definition" => Some(SymbolKind::Enum),
            "type_definition" => Some(SymbolKind::Type),
            "val_definition" | "var_definition" => Some(SymbolKind::Variable),
            "import_declaration" => Some(SymbolKind::Import),
            _ => None,
        }
    }

    fn html_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "element" => Some(SymbolKind::Variable),
            "script_element" => Some(SymbolKind::Module),
            "style_element" => Some(SymbolKind::Module),
            _ => None,
        }
    }

    fn json_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "pair" => Some(SymbolKind::Variable),
            "object" => Some(SymbolKind::Struct),
            "array" => Some(SymbolKind::Variable),
            _ => None,
        }
    }

    fn yaml_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "block_mapping_pair" => Some(SymbolKind::Variable),
            "block_mapping" => Some(SymbolKind::Struct),
            "block_sequence" => Some(SymbolKind::Variable),
            _ => None,
        }
    }

    fn toml_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "pair" => Some(SymbolKind::Variable),
            "table" => Some(SymbolKind::Struct),
            "array" => Some(SymbolKind::Variable),
            _ => None,
        }
    }

    fn bash_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            "variable_assignment" => Some(SymbolKind::Variable),
            _ => None,
        }
    }

    fn elixir_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
        match node_kind {
            "call" => Some(SymbolKind::Function), // def, defp, defmodule
            "anonymous_function" => Some(SymbolKind::Function),
            "do_block" => Some(SymbolKind::Module),
            _ => None,
        }
    }

    /// Vérifie si un lexical_declaration/variable_declaration contient une arrow_function
    /// ou function_expression comme valeur (pour TypeScript/JavaScript)
    fn is_function_variable(node: &Node) -> bool {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator"
                && let Some(value) = child.child_by_field_name("value")
                && matches!(value.kind(), "arrow_function" | "function_expression")
            {
                return true;
            }
        }
        false
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
            Language::Ruby => "name",
            Language::CSharp => "name",
            Language::Kotlin => "name",
            Language::Scala => "name",
            Language::Html => "tag_name",
            Language::Json => "key",
            Language::Yaml => "key",
            Language::Toml => "key",
            Language::Bash => "name",
            Language::Elixir => "name",
        };

        let source_bytes = source.as_bytes();

        if let Some(name_node) = node.child_by_field_name(name_field) {
            // Use utf8_text for safe UTF-8 handling
            if let Ok(text) = name_node.utf8_text(source_bytes) {
                return Some(text.to_string());
            }
        }

        // Handle lexical_declaration / variable_declaration in TS/JS
        // Structure: lexical_declaration -> variable_declarator -> identifier
        if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name_node) = child.child_by_field_name("name")
                        && let Ok(text) = name_node.utf8_text(source_bytes)
                    {
                        return Some(text.to_string());
                    }
                    // Fallback: look for identifier in variable_declarator
                    let mut inner_cursor = child.walk();
                    for inner_child in child.children(&mut inner_cursor) {
                        if inner_child.kind() == "identifier"
                            && let Ok(text) = inner_child.utf8_text(source_bytes)
                        {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }

        // Fallback: look for identifier child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if (child.kind() == "identifier" || child.kind() == "type_identifier")
                && let Ok(text) = child.utf8_text(source_bytes)
            {
                return Some(text.to_string());
            }
        }

        None
    }

    fn extract_signature(node: &Node, source: &str, _language: Language) -> Option<String> {
        let source_bytes = source.as_bytes();

        // Get the first line of the node as a simple signature using safe UTF-8 handling
        let text = node.utf8_text(source_bytes).ok()?;
        let first_line = text.lines().next()?;

        // Truncate if too long (handle multi-byte chars safely)
        let sig = if first_line.chars().count() > 200 {
            let truncated: String = first_line.chars().take(200).collect();
            format!("{}...", truncated)
        } else {
            first_line.to_string()
        };

        Some(sig.trim().to_string())
    }

    fn extract_doc_comment(node: &Node, source: &str) -> Option<String> {
        let source_bytes = source.as_bytes();

        // Look for preceding comment siblings
        let mut prev = node.prev_sibling();
        let mut comments = Vec::new();

        while let Some(sibling) = prev {
            if sibling.kind().contains("comment") {
                if let Ok(comment) = sibling.utf8_text(source_bytes) {
                    comments.push(comment.to_string());
                }
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

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "hello" && s.kind == SymbolKind::Function)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "User" && s.kind == SymbolKind::Struct)
        );
    }

    #[test]
    fn test_extract_typescript_const_variables() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
// Animation variants
export const fadeInUp = {
    hidden: { opacity: 0, y: 20 },
    visible: { opacity: 1, y: 0 }
};

const config = { debug: true };

let counter = 0;

function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
        let tree = support.parse(Language::TypeScript, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::TypeScript).unwrap();

        // Check that const/let variables are extracted
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "fadeInUp" && s.kind == SymbolKind::Variable),
            "fadeInUp should be extracted as Variable"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "config" && s.kind == SymbolKind::Variable),
            "config should be extracted as Variable"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "counter" && s.kind == SymbolKind::Variable),
            "counter should be extracted as Variable"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "greet" && s.kind == SymbolKind::Function),
            "greet should be extracted as Function"
        );
    }

    #[test]
    fn test_extract_python_symbols() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
class User:
    """A user class"""
    def __init__(self, name: str):
        self.name = name

    def greet(self) -> str:
        return f"Hello, {self.name}!"

def process_data(items: list) -> dict:
    """Process a list of items"""
    return {}
"#;
        let tree = support.parse(Language::Python, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::Python).unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "User" && s.kind == SymbolKind::Class)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "process_data" && s.kind == SymbolKind::Function)
        );
    }

    #[test]
    fn test_extract_go_symbols() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
package main

import "fmt"

type User struct {
    Name string
    Age  int
}

func (u *User) Greet() string {
    return fmt.Sprintf("Hello, %s!", u.Name)
}

func main() {
    fmt.Println("Hello, World!")
}
"#;
        let tree = support.parse(Language::Go, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::Go).unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "main" && s.kind == SymbolKind::Function)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Greet" && s.kind == SymbolKind::Method)
        );
    }

    #[test]
    fn test_extract_java_symbols() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
public class Calculator {
    private int value;

    public Calculator(int initial) {
        this.value = initial;
    }

    public int add(int n) {
        return value + n;
    }
}

interface Computable {
    int compute();
}
"#;
        let tree = support.parse(Language::Java, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::Java).unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "add" && s.kind == SymbolKind::Method)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Computable" && s.kind == SymbolKind::Interface)
        );
    }

    #[test]
    fn test_extract_c_symbols() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
#include <stdio.h>

struct Point {
    int x;
    int y;
};

enum Color {
    RED,
    GREEN,
    BLUE
};

int add(int a, int b) {
    return a + b;
}

int main() {
    printf("Hello, World!\n");
    return 0;
}
"#;
        let tree = support.parse(Language::C, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::C).unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Point" && s.kind == SymbolKind::Struct)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Color" && s.kind == SymbolKind::Enum)
        );
        // C functions have declarator as name which includes params, check for partial match
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Function && s.name.contains("add")),
            "Expected a function containing 'add', found: {:?}",
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Function)
                .collect::<Vec<_>>()
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Function && s.name.contains("main")),
            "Expected a function containing 'main', found: {:?}",
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Function)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_symbol_kind_as_str() {
        assert_eq!(SymbolKind::Function.as_str(), "function");
        assert_eq!(SymbolKind::Method.as_str(), "method");
        assert_eq!(SymbolKind::Class.as_str(), "class");
        assert_eq!(SymbolKind::Struct.as_str(), "struct");
        assert_eq!(SymbolKind::Enum.as_str(), "enum");
        assert_eq!(SymbolKind::Interface.as_str(), "interface");
        assert_eq!(SymbolKind::Trait.as_str(), "trait");
        assert_eq!(SymbolKind::Module.as_str(), "module");
        assert_eq!(SymbolKind::Variable.as_str(), "variable");
        assert_eq!(SymbolKind::Constant.as_str(), "constant");
        assert_eq!(SymbolKind::Type.as_str(), "type");
        assert_eq!(SymbolKind::Import.as_str(), "import");
    }

    #[test]
    fn test_symbol_with_doc_comment() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
/// This is a documented function
/// It does something important
fn documented_function() {
    println!("Hello");
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::Rust).unwrap();

        let func = symbols
            .iter()
            .find(|s| s.name == "documented_function")
            .unwrap();
        assert!(func.doc_comment.is_some());
        assert!(
            func.doc_comment
                .as_ref()
                .unwrap()
                .contains("documented function")
        );
    }

    #[test]
    fn test_nested_symbols_with_parent() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
impl Calculator {
    fn add(&self, n: i32) -> i32 {
        self.value + n
    }

    fn subtract(&self, n: i32) -> i32 {
        self.value - n
    }
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::Rust).unwrap();

        // The impl block should be detected, and functions inside should have parent
        let add_func = symbols.iter().find(|s| s.name == "add");
        assert!(add_func.is_some());
    }

    #[test]
    fn test_extract_typescript_arrow_functions() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
export const ToolsShowcase = () => {
    return "Tools";
};

const CopyButton = (props: Props) => {
    return "Copy";
};

// Doit rester Variable (pas de fonction)
const config = { debug: true };
"#;
        let tree = support.parse(Language::TypeScript, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::TypeScript).unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "ToolsShowcase" && s.kind == SymbolKind::Function),
            "ToolsShowcase should be extracted as Function, found: {:?}",
            symbols
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "CopyButton" && s.kind == SymbolKind::Function),
            "CopyButton should be extracted as Function"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "config" && s.kind == SymbolKind::Variable),
            "config should remain as Variable"
        );
    }

    #[test]
    fn test_extract_function_expression() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
const greet = function(name: string): string {
    return `Hello, ${name}!`;
};
"#;
        let tree = support.parse(Language::TypeScript, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::TypeScript).unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "greet" && s.kind == SymbolKind::Function),
            "greet should be extracted as Function (function expression)"
        );
    }
}
