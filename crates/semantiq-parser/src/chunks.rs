use crate::language::Language;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tree_sitter::Tree;

const DEFAULT_CHUNK_SIZE: usize = 1500;
const OVERLAP_LINES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub symbols: Vec<String>,
}

pub struct ChunkExtractor {
    chunk_size: usize,
}

impl ChunkExtractor {
    pub fn new() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }

    pub fn with_chunk_size(mut self, size: usize) -> Self {
        self.chunk_size = size;
        self
    }

    pub fn extract(&self, tree: &Tree, source: &str, language: Language) -> Result<Vec<CodeChunk>> {
        let lines: Vec<&str> = source.lines().collect();
        let mut chunks = Vec::new();
        let root = tree.root_node();

        // Find semantic boundaries (functions, classes, etc.)
        let boundaries = self.find_semantic_boundaries(&root, source, language);

        if boundaries.is_empty() {
            // Fall back to line-based chunking
            return Ok(self.line_based_chunks(source, &lines));
        }

        // Create chunks at semantic boundaries
        let mut current_start = 0;
        let mut current_symbols = Vec::new();

        for boundary in boundaries {
            let boundary_line = boundary.start_line;

            if boundary_line > current_start {
                // Check if we need to create a chunk
                let content_size: usize = lines[current_start..boundary_line]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum();

                if content_size >= self.chunk_size && !current_symbols.is_empty() {
                    // Create a chunk
                    let chunk = self.create_chunk(source, &lines, current_start, boundary_line, &current_symbols);
                    chunks.push(chunk);
                    current_start = boundary_line.saturating_sub(OVERLAP_LINES);
                    current_symbols.clear();
                }
            }

            current_symbols.push(boundary.name.clone());
        }

        // Handle remaining content
        if current_start < lines.len() {
            let chunk = self.create_chunk(source, &lines, current_start, lines.len(), &current_symbols);
            chunks.push(chunk);
        }

        Ok(chunks)
    }

    fn find_semantic_boundaries(&self, node: &tree_sitter::Node, source: &str, language: Language) -> Vec<SemanticBoundary> {
        let mut boundaries = Vec::new();
        self.collect_boundaries(node, source, language, &mut boundaries);
        boundaries.sort_by_key(|b| b.start_line);
        boundaries
    }

    fn collect_boundaries(
        &self,
        node: &tree_sitter::Node,
        source: &str,
        language: Language,
        boundaries: &mut Vec<SemanticBoundary>,
    ) {
        if self.is_boundary_node(node.kind(), language) {
            if let Some(name) = self.get_node_name(node, source) {
                boundaries.push(SemanticBoundary {
                    name,
                    start_line: node.start_position().row,
                    end_line: node.end_position().row,
                });
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.collect_boundaries(&child, source, language, boundaries);
        }
    }

    fn is_boundary_node(&self, kind: &str, language: Language) -> bool {
        match language {
            Language::Rust => matches!(
                kind,
                "function_item" | "struct_item" | "enum_item" | "trait_item" | "impl_item" | "mod_item"
            ),
            Language::TypeScript | Language::JavaScript => matches!(
                kind,
                "function_declaration" | "class_declaration" | "interface_declaration" | "method_definition"
            ),
            Language::Python => matches!(kind, "function_definition" | "class_definition"),
            Language::Go => matches!(
                kind,
                "function_declaration" | "method_declaration" | "type_declaration"
            ),
            Language::Java => matches!(
                kind,
                "method_declaration" | "class_declaration" | "interface_declaration"
            ),
            Language::C | Language::Cpp => matches!(
                kind,
                "function_definition" | "struct_specifier" | "class_specifier"
            ),
            Language::Php => matches!(
                kind,
                "function_definition" | "method_declaration" | "class_declaration" | "interface_declaration" | "trait_declaration"
            ),
        }
    }

    fn get_node_name(&self, node: &tree_sitter::Node, source: &str) -> Option<String> {
        let source_bytes = source.as_bytes();

        // Try common name fields
        for field in &["name", "declarator"] {
            if let Some(name_node) = node.child_by_field_name(field) {
                if let Ok(text) = name_node.utf8_text(source_bytes) {
                    return Some(text.to_string());
                }
            }
        }

        // Fallback to looking for identifier
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "identifier" || child.kind() == "type_identifier" {
                if let Ok(text) = child.utf8_text(source_bytes) {
                    return Some(text.to_string());
                }
            }
        }

        None
    }

    fn create_chunk(
        &self,
        source: &str,
        lines: &[&str],
        start_line: usize,
        end_line: usize,
        symbols: &[String],
    ) -> CodeChunk {
        let end_line = end_line.min(lines.len());
        let content = lines[start_line..end_line].join("\n");

        // Calculate byte positions
        let start_byte: usize = lines[..start_line].iter().map(|l| l.len() + 1).sum();
        let end_byte = start_byte + content.len();

        CodeChunk {
            content,
            start_line: start_line + 1,
            end_line,
            start_byte: start_byte.min(source.len()),
            end_byte: end_byte.min(source.len()),
            symbols: symbols.to_vec(),
        }
    }

    fn line_based_chunks(&self, source: &str, lines: &[&str]) -> Vec<CodeChunk> {
        let mut chunks = Vec::new();
        let mut current_start = 0;

        while current_start < lines.len() {
            let mut current_size = 0;
            let mut current_end = current_start;

            while current_end < lines.len() && current_size < self.chunk_size {
                current_size += lines[current_end].len() + 1;
                current_end += 1;
            }

            let chunk = self.create_chunk(source, lines, current_start, current_end, &[]);
            chunks.push(chunk);

            current_start = current_end.saturating_sub(OVERLAP_LINES);
            if current_start <= chunks.last().map(|c| c.start_line - 1).unwrap_or(0) {
                current_start = current_end;
            }
        }

        chunks
    }
}

impl Default for ChunkExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct SemanticBoundary {
    name: String,
    start_line: usize,
    end_line: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::LanguageSupport;

    #[test]
    fn test_chunk_extraction() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
fn foo() {
    println!("foo");
}

fn bar() {
    println!("bar");
}

fn baz() {
    println!("baz");
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = ChunkExtractor::new().with_chunk_size(50);
        let chunks = extractor.extract(&tree, source, Language::Rust).unwrap();

        assert!(!chunks.is_empty());
    }
}
