use crate::language::Language;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tree_sitter::Tree;

const DEFAULT_CHUNK_SIZE: usize = 1500;
const OVERLAP_LINES: usize = 3;
/// A span between symbol boundaries larger than this many times `chunk_size`
/// (one huge function, a big `match` dispatcher) is split into windows of
/// about `chunk_size` bytes, so no part of it stays beyond the embedding
/// model's token limit.
const SPLIT_FACTOR: usize = 2;

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
        let mut current_symbols: Vec<SemanticBoundary> = Vec::new();

        for boundary in boundaries {
            let boundary_line = boundary.start_line;

            if boundary_line > current_start {
                // Check if we need to create a chunk
                let content_size: usize = lines[current_start..boundary_line]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum();

                if content_size >= self.chunk_size && !current_symbols.is_empty() {
                    self.push_span(
                        source,
                        &lines,
                        current_start,
                        boundary_line,
                        &current_symbols,
                        &mut chunks,
                    );
                    current_start = boundary_line.saturating_sub(OVERLAP_LINES);
                    current_symbols.clear();
                }
            }

            current_symbols.push(boundary);
        }

        // Handle remaining content
        if current_start < lines.len() {
            self.push_span(
                source,
                &lines,
                current_start,
                lines.len(),
                &current_symbols,
                &mut chunks,
            );
        }

        Ok(chunks)
    }

    /// Emit the span `start_line..end_line` (0-based, exclusive) as one chunk,
    /// or as overlapping windows of about `chunk_size` bytes when it is larger
    /// than `SPLIT_FACTOR * chunk_size`. Each window lists the symbols whose
    /// definition overlaps it (the enclosing function of a window deep inside
    /// a long body), falling back to every symbol of the span.
    fn push_span(
        &self,
        source: &str,
        lines: &[&str],
        start_line: usize,
        end_line: usize,
        symbols: &[SemanticBoundary],
        chunks: &mut Vec<CodeChunk>,
    ) {
        let end_line = end_line.min(lines.len());
        let names: Vec<String> = symbols.iter().map(|b| b.name.clone()).collect();
        let size: usize = lines[start_line..end_line]
            .iter()
            .map(|l| l.len() + 1)
            .sum();
        if size <= SPLIT_FACTOR * self.chunk_size {
            chunks.push(Self::create_chunk(
                source, lines, start_line, end_line, &names,
            ));
            return;
        }

        for (window_start, window_end) in self.windows(lines, start_line, end_line) {
            let mut window_symbols: Vec<String> = symbols
                .iter()
                .filter(|b| b.start_line < window_end && window_start <= b.end_line)
                .map(|b| b.name.clone())
                .collect();
            if window_symbols.is_empty() {
                window_symbols = names.clone();
            }
            chunks.push(Self::create_chunk(
                source,
                lines,
                window_start,
                window_end,
                &window_symbols,
            ));
        }
    }

    /// Windows of about `chunk_size` bytes over `lines[start_line..end_line]`
    /// (0-based, exclusive), as `(start, end)` pairs. Consecutive windows
    /// overlap by `OVERLAP_LINES`, capped at half the window so that long
    /// lines (minified JS, generated tables, long literals: windows of 1–4
    /// lines) do not duplicate most of the content and its embeddings.
    fn windows(&self, lines: &[&str], start_line: usize, end_line: usize) -> Vec<(usize, usize)> {
        let end_line = end_line.min(lines.len());
        let mut windows = Vec::new();
        let mut window_start = start_line;
        while window_start < end_line {
            let mut window_end = window_start;
            let mut window_size = 0;
            while window_end < end_line && window_size < self.chunk_size {
                window_size += lines[window_end].len() + 1;
                window_end += 1;
            }
            windows.push((window_start, window_end));
            if window_end >= end_line {
                break;
            }
            let overlap = OVERLAP_LINES.min((window_end - window_start) / 2);
            window_start = window_end - overlap;
        }
        windows
    }

    fn find_semantic_boundaries(
        &self,
        node: &tree_sitter::Node,
        source: &str,
        language: Language,
    ) -> Vec<SemanticBoundary> {
        let mut boundaries = Vec::new();
        Self::collect_boundaries(node, source, language, &mut boundaries);
        boundaries.sort_by_key(|b| b.start_line);
        boundaries
    }

    fn collect_boundaries(
        node: &tree_sitter::Node,
        source: &str,
        language: Language,
        boundaries: &mut Vec<SemanticBoundary>,
    ) {
        if Self::is_boundary_node(node.kind(), language)
            && let Some(name) = Self::get_node_name(node, source)
        {
            boundaries.push(SemanticBoundary {
                name,
                start_line: node.start_position().row,
                end_line: node.end_position().row,
            });
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_boundaries(&child, source, language, boundaries);
        }
    }

    fn is_boundary_node(kind: &str, language: Language) -> bool {
        match language {
            Language::Rust => matches!(
                kind,
                "function_item"
                    | "struct_item"
                    | "enum_item"
                    | "trait_item"
                    | "impl_item"
                    | "mod_item"
            ),
            Language::TypeScript | Language::JavaScript => matches!(
                kind,
                "function_declaration"
                    | "arrow_function"
                    | "lexical_declaration"
                    | "class_declaration"
                    | "interface_declaration"
                    | "method_definition"
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
                "function_definition"
                    | "method_declaration"
                    | "class_declaration"
                    | "interface_declaration"
                    | "trait_declaration"
            ),
            Language::Ruby => matches!(kind, "method" | "singleton_method" | "class" | "module"),
            Language::CSharp => matches!(
                kind,
                "method_declaration"
                    | "class_declaration"
                    | "struct_declaration"
                    | "interface_declaration"
            ),
            Language::Kotlin => matches!(
                kind,
                "function_declaration"
                    | "class_declaration"
                    | "object_declaration"
                    | "interface_declaration"
            ),
            Language::Scala => matches!(
                kind,
                "function_definition"
                    | "class_definition"
                    | "object_definition"
                    | "trait_definition"
            ),
            Language::Html => matches!(kind, "element" | "script_element" | "style_element"),
            Language::Json => matches!(kind, "object" | "array"),
            Language::Yaml => matches!(kind, "block_mapping" | "block_sequence"),
            Language::Toml => matches!(kind, "table" | "array"),
            Language::Bash => matches!(kind, "function_definition" | "compound_statement"),
            Language::Elixir => matches!(kind, "call" | "anonymous_function" | "do_block"),
        }
    }

    fn get_node_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
        let source_bytes = source.as_bytes();

        // Try common name fields
        for field in &["name", "declarator"] {
            if let Some(name_node) = node.child_by_field_name(field)
                && let Ok(text) = name_node.utf8_text(source_bytes)
            {
                return Some(text.to_string());
            }
        }

        // Fallback to looking for identifier
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

    fn create_chunk(
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
        self.windows(lines, 0, lines.len())
            .into_iter()
            .map(|(start, end)| Self::create_chunk(source, lines, start, end, &[]))
            .collect()
    }
}

impl Default for ChunkExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// A symbol definition, lines 0-based inclusive.
#[derive(Debug)]
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

    #[test]
    fn test_chunk_extractor_default() {
        let extractor = ChunkExtractor::default();
        // Should use DEFAULT_CHUNK_SIZE = 1500
        assert_eq!(extractor.chunk_size, 1500);
    }

    #[test]
    fn test_chunk_extractor_with_chunk_size() {
        let extractor = ChunkExtractor::new().with_chunk_size(500);
        assert_eq!(extractor.chunk_size, 500);
    }

    #[test]
    fn test_chunk_has_correct_line_numbers() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"fn main() {
    println!("Hello");
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = ChunkExtractor::new();
        let chunks = extractor.extract(&tree, source, Language::Rust).unwrap();

        assert!(!chunks.is_empty());
        // Line numbers should be 1-indexed
        assert!(chunks[0].start_line >= 1);
    }

    #[test]
    fn test_chunk_contains_symbols() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
fn hello_world() {
    println!("Hello, World!");
}

fn goodbye_world() {
    println!("Goodbye, World!");
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = ChunkExtractor::new().with_chunk_size(50);
        let chunks = extractor.extract(&tree, source, Language::Rust).unwrap();

        // At least some chunks should contain symbol names
        let has_symbols = chunks.iter().any(|c| !c.symbols.is_empty());
        assert!(has_symbols || chunks.len() == 1); // Either has symbols or single chunk
    }

    #[test]
    fn test_chunk_byte_positions() {
        let mut support = LanguageSupport::new().unwrap();
        let source = "fn test() {}";
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = ChunkExtractor::new();
        let chunks = extractor.extract(&tree, source, Language::Rust).unwrap();

        assert!(!chunks.is_empty());
        // Byte positions should be valid
        assert!(chunks[0].start_byte <= chunks[0].end_byte);
        assert!(chunks[0].end_byte <= source.len());
    }

    #[test]
    fn test_line_based_chunking_fallback() {
        let mut support = LanguageSupport::new().unwrap();
        // Source without semantic boundaries
        let source = "// Just a comment\n// Another comment\n// More comments\n";
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = ChunkExtractor::new().with_chunk_size(20);
        let chunks = extractor.extract(&tree, source, Language::Rust).unwrap();

        // Should fall back to line-based chunking
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunk_extraction_typescript() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
function greet(name: string): string {
    return `Hello, ${name}!`;
}

class Calculator {
    add(a: number, b: number): number {
        return a + b;
    }
}
"#;
        let tree = support.parse(Language::TypeScript, source).unwrap();
        let extractor = ChunkExtractor::new().with_chunk_size(50);
        let chunks = extractor
            .extract(&tree, source, Language::TypeScript)
            .unwrap();

        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunk_extraction_python() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
def greet(name):
    return f"Hello, {name}!"

class Calculator:
    def add(self, a, b):
        return a + b
"#;
        let tree = support.parse(Language::Python, source).unwrap();
        let extractor = ChunkExtractor::new().with_chunk_size(50);
        let chunks = extractor.extract(&tree, source, Language::Python).unwrap();

        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunk_content_matches_source() {
        let mut support = LanguageSupport::new().unwrap();
        let source = "fn main() {\n    println!(\"Hello\");\n}";
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = ChunkExtractor::new();
        let chunks = extractor.extract(&tree, source, Language::Rust).unwrap();

        assert!(!chunks.is_empty());
        // The chunk content should be a substring of the source
        assert!(source.contains(&chunks[0].content) || chunks[0].content.contains("fn main"));
    }

    /// #62: a function larger than `chunk_size` is split into overlapping
    /// windows that keep the symbol metadata, instead of one oversized chunk
    /// whose embedding would only cover its first 512 tokens.
    #[test]
    fn test_oversized_function_is_split_into_windows() {
        let mut support = LanguageSupport::new().unwrap();
        let mut source = String::from("fn small() {}\n\nfn huge() {\n");
        for i in 0..400 {
            source.push_str(&format!("    let value_{i} = compute({i});\n"));
        }
        source.push_str("}\n\nfn after() {}\n");
        let tree = support.parse(Language::Rust, &source).unwrap();
        let extractor = ChunkExtractor::new();
        let chunks = extractor.extract(&tree, &source, Language::Rust).unwrap();

        let limit = SPLIT_FACTOR * extractor.chunk_size;
        assert!(chunks.len() > 5, "{} chunks", chunks.len());
        for chunk in &chunks {
            assert!(
                chunk.content.len() <= limit,
                "chunk L{}-{} has {} bytes",
                chunk.start_line,
                chunk.end_line,
                chunk.content.len()
            );
        }
        // Every window inside the body keeps `huge`; `small` only appears in
        // the first one, `after` only in the last one (whose overlap lines
        // carry the next span's symbols, as before).
        let body_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.content.contains("let value_") && !c.content.contains("fn after"))
            .collect();
        assert!(body_chunks.len() > 3);
        assert!(
            body_chunks
                .iter()
                .all(|c| c.symbols.contains(&"huge".to_string()))
        );
        assert!(chunks[0].symbols.contains(&"small".to_string()));
        assert!(!chunks[2].symbols.contains(&"small".to_string()));
        assert!(
            chunks
                .last()
                .unwrap()
                .symbols
                .contains(&"after".to_string())
        );
        // Windows are contiguous or overlapping and cover the whole file.
        assert_eq!(chunks[0].start_line, 1);
        for pair in chunks.windows(2) {
            assert!(pair[1].start_line > pair[0].start_line);
            assert!(pair[1].start_line <= pair[0].end_line + 1);
        }
        assert_eq!(chunks.last().unwrap().end_line, source.lines().count());
        // Chunk keys stay unique for dedup.
        let mut keys: Vec<_> = chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
        keys.dedup();
        assert_eq!(keys.len(), chunks.len());
    }

    /// Long lines (minified JS, generated tables): windows of a few lines
    /// must not overlap by more than half their size, or the content and its
    /// embeddings are duplicated several times over the span.
    #[test]
    fn test_long_lines_do_not_duplicate_content_through_overlap() {
        let mut support = LanguageSupport::new().unwrap();
        let mut source = String::from("fn table() {\n");
        let long_line = format!("    let row = \"{}\";\n", "x".repeat(580));
        for _ in 0..200 {
            source.push_str(&long_line);
        }
        source.push_str("}\n");
        let tree = support.parse(Language::Rust, &source).unwrap();
        let extractor = ChunkExtractor::new();
        let chunks = extractor.extract(&tree, &source, Language::Rust).unwrap();

        let lines = source.lines().count();
        // 3 lines of ~600 bytes per window, overlap capped at 1: ~2 new lines
        // per window instead of 1 (overlap 3 would advance one line at a time).
        assert!(chunks.len() <= lines / 2 + 2, "{} chunks", chunks.len());
        let stored: usize = chunks.iter().map(|c| c.content.len()).sum();
        assert!(
            stored <= 2 * source.len(),
            "{stored} bytes stored for {}",
            source.len()
        );
        assert_eq!(chunks.last().unwrap().end_line, lines);
        for pair in chunks.windows(2) {
            assert!(pair[1].start_line > pair[0].start_line);
            assert!(pair[1].start_line <= pair[0].end_line + 1);
        }
    }

    #[test]
    fn test_line_based_chunks_overlap() {
        let mut support = LanguageSupport::new().unwrap();
        let source: String = (0..40).map(|i| format!("// line {i}\n")).collect();
        let tree = support.parse(Language::Rust, &source).unwrap();
        let chunks = ChunkExtractor::new()
            .with_chunk_size(100)
            .extract(&tree, &source, Language::Rust)
            .unwrap();
        assert!(chunks.len() > 3);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks.last().unwrap().end_line, 40);
        for pair in chunks.windows(2) {
            let overlap = pair[0].end_line + 1 - pair[1].start_line;
            assert!((1..=OVERLAP_LINES).contains(&overlap), "overlap {overlap}");
        }
    }

    #[test]
    fn test_span_below_split_threshold_stays_whole() {
        let mut support = LanguageSupport::new().unwrap();
        let mut source = String::from("fn first() {\n");
        for i in 0..20 {
            source.push_str(&format!("    let v{i} = {i};\n"));
        }
        source.push_str("}\nfn second() {}\n");
        let tree = support.parse(Language::Rust, &source).unwrap();
        // Larger than chunk_size but below SPLIT_FACTOR * chunk_size: one chunk.
        let extractor = ChunkExtractor::new().with_chunk_size(source.len() * 2 / 3);
        let chunks = extractor.extract(&tree, &source, Language::Rust).unwrap();
        assert_eq!(chunks.len(), 2, "{chunks:#?}");
        assert_eq!(chunks[0].symbols, vec!["first".to_string()]);
    }

    #[test]
    fn test_empty_source() {
        let mut support = LanguageSupport::new().unwrap();
        let source = "";
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = ChunkExtractor::new();
        let chunks = extractor.extract(&tree, source, Language::Rust).unwrap();

        // Empty source should produce no or empty chunks
        assert!(chunks.is_empty() || chunks[0].content.is_empty());
    }
}
