use crate::query::Query;
use crate::results::{SearchResult, SearchResultKind, SearchResultMetadata, SearchResults};
use anyhow::Result;
use ignore::WalkBuilder;
use semantiq_index::IndexStore;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct RetrievalEngine {
    store: Arc<IndexStore>,
    root_path: String,
}

impl RetrievalEngine {
    pub fn new(store: Arc<IndexStore>, root_path: &str) -> Self {
        Self {
            store,
            root_path: root_path.to_string(),
        }
    }

    pub fn search(&self, query_text: &str, limit: usize) -> Result<SearchResults> {
        let start = Instant::now();
        let query = Query::new(query_text);

        // Cap limit to prevent excessive memory usage
        let safe_limit = limit.min(500);

        let mut all_results = Vec::new();

        // 1. Symbol search (FTS) - prioritize symbol matches
        let symbol_results = self.search_symbols(&query, safe_limit)?;
        all_results.extend(symbol_results);

        // 2. Text search (grep-like) - only if we need more results
        if all_results.len() < safe_limit {
            let text_results = self.search_text(&query, safe_limit - all_results.len())?;
            all_results.extend(text_results);
        }

        // Sort by score (highest first), use total_cmp for safe NaN handling
        all_results.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Remove duplicates based on file_path + start_line + content hash
        let mut seen = std::collections::HashSet::new();
        all_results.retain(|r| {
            let key = format!("{}:{}:{}", r.file_path, r.start_line, r.content.len());
            seen.insert(key)
        });

        // Limit results
        all_results.truncate(safe_limit);

        let search_time = start.elapsed().as_millis() as u64;
        Ok(SearchResults::new(
            query_text.to_string(),
            all_results,
            search_time,
        ))
    }

    pub fn find_references(&self, symbol_name: &str, limit: usize) -> Result<SearchResults> {
        let start = Instant::now();
        let mut results = Vec::new();

        // Find symbol definitions
        let symbols = self.store.find_symbol_by_name(symbol_name)?;

        for symbol in &symbols {
            if let Some(file) = self.store.get_file_by_path(&self.get_file_path(symbol.file_id)?)? {
                let content = self.read_file_lines(&file.path, symbol.start_line as usize, symbol.end_line as usize)?;

                results.push(SearchResult::new(
                    SearchResultKind::Symbol,
                    file.path.clone(),
                    symbol.start_line as usize,
                    symbol.end_line as usize,
                    content,
                    1.0,
                ).with_metadata(SearchResultMetadata {
                    symbol_name: Some(symbol.name.clone()),
                    symbol_kind: Some(symbol.kind.clone()),
                    match_type: Some("definition".to_string()),
                    context: symbol.signature.clone(),
                }));
            }
        }

        // Find usages via text search
        let usage_results = self.search_text(&Query::new(symbol_name), limit)?;
        for mut result in usage_results {
            result.kind = SearchResultKind::Reference;
            result.metadata.match_type = Some("usage".to_string());
            results.push(result);
        }

        results.truncate(limit);

        let search_time = start.elapsed().as_millis() as u64;
        Ok(SearchResults::new(
            symbol_name.to_string(),
            results,
            search_time,
        ))
    }

    pub fn get_dependencies(&self, file_path: &str) -> Result<Vec<DependencyInfo>> {
        let mut deps = Vec::new();

        if let Some(file) = self.store.get_file_by_path(file_path)? {
            let records = self.store.get_dependencies(file.id)?;

            for record in records {
                deps.push(DependencyInfo {
                    target_path: record.target_path,
                    import_name: record.import_name,
                    kind: record.kind,
                });
            }
        }

        Ok(deps)
    }

    pub fn get_dependents(&self, file_path: &str) -> Result<Vec<DependencyInfo>> {
        let mut deps = Vec::new();

        let records = self.store.get_dependents(file_path)?;

        for record in records {
            let source_path = self.get_file_path(record.source_file_id)?;
            deps.push(DependencyInfo {
                target_path: source_path,
                import_name: record.import_name,
                kind: record.kind,
            });
        }

        Ok(deps)
    }

    pub fn explain_symbol(&self, symbol_name: &str) -> Result<SymbolExplanation> {
        let symbols = self.store.find_symbol_by_name(symbol_name)?;

        if symbols.is_empty() {
            return Ok(SymbolExplanation {
                name: symbol_name.to_string(),
                found: false,
                definitions: Vec::new(),
                usage_count: 0,
                related_symbols: Vec::new(),
            });
        }

        let mut definitions = Vec::new();
        let mut related_symbols = std::collections::HashSet::new();

        for symbol in &symbols {
            let file_path = self.get_file_path(symbol.file_id)?;

            definitions.push(SymbolDefinition {
                file_path: file_path.clone(),
                kind: symbol.kind.clone(),
                start_line: symbol.start_line as usize,
                end_line: symbol.end_line as usize,
                signature: symbol.signature.clone(),
                doc_comment: symbol.doc_comment.clone(),
            });

            // Find related symbols in the same file
            let file_symbols = self.store.get_symbols_by_file(symbol.file_id)?;
            for fs in file_symbols {
                if fs.name != symbol_name {
                    related_symbols.insert(fs.name);
                }
            }
        }

        // Count usages
        let usage_results = self.search_text(&Query::new(symbol_name), 100)?;
        let usage_count = usage_results.len();

        Ok(SymbolExplanation {
            name: symbol_name.to_string(),
            found: true,
            definitions,
            usage_count,
            related_symbols: related_symbols.into_iter().collect(),
        })
    }

    // Private helper methods

    fn search_symbols(&self, query: &Query, limit: usize) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();

        for term in query.all_terms() {
            let symbols = self.store.search_symbols(term, limit)?;

            for symbol in symbols {
                let file_path = self.get_file_path(symbol.file_id)?;
                let content = symbol.signature.clone().unwrap_or_else(|| symbol.name.clone());

                // Improved scoring algorithm
                let name_lower = symbol.name.to_lowercase();
                let term_lower = term.to_lowercase();

                let mut score = if name_lower == term_lower {
                    1.0 // Exact match
                } else if name_lower.starts_with(&term_lower) {
                    0.85 // Prefix match
                } else if name_lower.contains(&term_lower) {
                    0.7 // Contains match
                } else {
                    0.5 // FTS match
                };

                // Boost score based on symbol kind (functions/methods are usually more important)
                let kind_boost = match symbol.kind.as_str() {
                    "function" | "method" => 1.15,
                    "class" | "struct" | "trait" | "interface" => 1.1,
                    "enum" | "type" => 1.05,
                    "module" => 1.0,
                    "constant" => 0.95,
                    "variable" => 0.9,
                    _ => 1.0,
                };
                score *= kind_boost;

                // Slight boost for shorter names (more specific matches)
                let length_factor = 1.0 + (1.0 / (symbol.name.len() as f32 + 5.0));
                score *= length_factor;

                // Cap score at 1.0
                score = score.min(1.0);

                results.push(
                    SearchResult::new(
                        SearchResultKind::Symbol,
                        file_path,
                        symbol.start_line as usize,
                        symbol.end_line as usize,
                        content,
                        score,
                    )
                    .with_metadata(SearchResultMetadata {
                        symbol_name: Some(symbol.name),
                        symbol_kind: Some(symbol.kind),
                        match_type: Some("symbol".to_string()),
                        context: symbol.doc_comment,
                    }),
                );
            }
        }

        Ok(results)
    }

    fn search_text(&self, query: &Query, limit: usize) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();
        let root = Path::new(&self.root_path);

        if !root.exists() {
            return Ok(results);
        }

        let walker = WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .build();

        for entry in walker.filter_map(|e| e.ok()) {
            if results.len() >= limit {
                break;
            }

            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Skip non-code files
            if !Self::is_code_file(path) {
                continue;
            }

            if let Ok(content) = fs::read_to_string(path) {
                let matches = self.find_text_matches(&content, query);

                for (line_num, line_content, score) in matches {
                    let rel_path = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .to_string();

                    results.push(SearchResult::new(
                        SearchResultKind::TextMatch,
                        rel_path,
                        line_num,
                        line_num,
                        line_content,
                        score,
                    ));

                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }

        Ok(results)
    }

    fn find_text_matches(&self, content: &str, query: &Query) -> Vec<(usize, String, f32)> {
        let mut matches = Vec::new();
        let terms = query.all_terms();

        for (line_num, line) in content.lines().enumerate() {
            let line_lower = line.to_lowercase();
            let line_trimmed = line.trim();

            // Skip empty lines and comments
            if line_trimmed.is_empty() || line_trimmed.starts_with("//") || line_trimmed.starts_with('#') {
                continue;
            }

            for term in &terms {
                let term_lower = term.to_lowercase();
                if let Some(pos) = line_lower.find(&term_lower) {
                    // Improved scoring based on match quality
                    let mut score = if line_lower.trim() == term_lower {
                        0.9 // Exact line match (but lower than symbol matches)
                    } else if pos == 0 || !line_lower.chars().nth(pos.saturating_sub(1)).map(|c| c.is_alphanumeric()).unwrap_or(false) {
                        // Word boundary match (higher score)
                        0.7
                    } else {
                        // Substring match
                        0.5
                    };

                    // Boost if match is near the beginning of the line
                    let position_factor = 1.0 - (pos as f32 / (line.len() as f32 + 10.0)) * 0.2;
                    score *= position_factor;

                    matches.push((line_num + 1, line_trimmed.to_string(), score));
                    break;
                }
            }
        }

        matches
    }

    fn get_file_path(&self, file_id: i64) -> Result<String> {
        self.store
            .get_file_path_by_id(file_id)?
            .ok_or_else(|| anyhow::anyhow!("File not found with id: {}", file_id))
    }

    fn read_file_lines(&self, file_path: &str, start: usize, end: usize) -> Result<String> {
        let full_path = Path::new(&self.root_path).join(file_path);
        let content = fs::read_to_string(full_path)?;
        let lines: Vec<&str> = content.lines().collect();

        let start_idx = start.saturating_sub(1);
        let end_idx = end.min(lines.len());

        Ok(lines[start_idx..end_idx].join("\n"))
    }

    fn is_code_file(path: &Path) -> bool {
        let code_extensions = [
            "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "cpp", "cc", "h", "hpp",
            "rb", "php", "cs", "swift", "kt", "scala", "vue", "svelte",
        ];

        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| code_extensions.contains(&ext.to_lowercase().as_str()))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct DependencyInfo {
    pub target_path: String,
    pub import_name: Option<String>,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct SymbolExplanation {
    pub name: String,
    pub found: bool,
    pub definitions: Vec<SymbolDefinition>,
    pub usage_count: usize,
    pub related_symbols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SymbolDefinition {
    pub file_path: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_code_file() {
        assert!(RetrievalEngine::is_code_file(Path::new("test.rs")));
        assert!(RetrievalEngine::is_code_file(Path::new("app.tsx")));
        assert!(!RetrievalEngine::is_code_file(Path::new("readme.md")));
    }
}
