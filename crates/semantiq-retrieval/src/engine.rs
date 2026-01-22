use crate::query::{Query, SearchOptions};
use crate::results::{SearchResult, SearchResultKind, SearchResultMetadata, SearchResults};
use crate::text_searcher::TextSearcher;
use anyhow::Result;
use ignore::WalkBuilder;
use semantiq_embeddings::{EmbeddingModel, create_embedding_model};
use semantiq_index::IndexStore;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info};

pub struct RetrievalEngine {
    store: Arc<IndexStore>,
    root_path: String,
    embedding_model: Option<Box<dyn EmbeddingModel>>,
}

impl RetrievalEngine {
    pub fn new(store: Arc<IndexStore>, root_path: &str) -> Self {
        // Try to load embedding model
        let embedding_model = match create_embedding_model(None) {
            Ok(model) => {
                debug!("Embedding model loaded (dim={})", model.dimension());
                Some(model)
            }
            Err(e) => {
                debug!("Failed to load embedding model: {}", e);
                None
            }
        };

        Self {
            store,
            root_path: root_path.to_string(),
            embedding_model,
        }
    }

    pub fn search(
        &self,
        query_text: &str,
        limit: usize,
        options: Option<SearchOptions>,
    ) -> Result<SearchResults> {
        let start = Instant::now();
        let query = Query::new(query_text);
        let opts = options.unwrap_or_default();

        // Cap limit to prevent excessive memory usage
        let safe_limit = limit.min(500);

        let mut all_results = Vec::new();

        // 1. Semantic search (vector similarity) - highest priority
        if self.embedding_model.is_some() {
            let semantic_results = self.search_semantic(query_text, safe_limit, &opts)?;
            all_results.extend(semantic_results);
        }

        // 2. Symbol search (FTS) - prioritize symbol matches
        let symbol_results = self.search_symbols(&query, safe_limit, &opts)?;
        all_results.extend(symbol_results);

        // 3. Text search (grep-like) - only if we need more results
        if all_results.len() < safe_limit {
            let text_results = self.search_text(&query, safe_limit - all_results.len(), &opts)?;
            all_results.extend(text_results);
        }

        // Sort by score (highest first), use total_cmp for safe NaN handling
        all_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Remove duplicates based on file_path + start_line + content hash
        let mut seen = std::collections::HashSet::new();
        all_results.retain(|r| {
            let key = format!("{}:{}:{}", r.file_path, r.start_line, r.content.len());
            seen.insert(key)
        });

        // Filter by minimum score
        let min_score = opts.effective_min_score();
        all_results.retain(|r| r.score >= min_score);

        // Limit results
        all_results.truncate(safe_limit);

        let search_time = start.elapsed().as_millis() as u64;
        info!(
            query = %query_text,
            results = all_results.len(),
            time_ms = search_time,
            "Search completed"
        );
        Ok(SearchResults::new(
            query_text.to_string(),
            all_results,
            search_time,
        ))
    }

    /// Minimum similarity threshold for semantic search results.
    /// Results with similarity below this threshold are excluded.
    /// sqlite-vec uses L2 distance, so lower distance = more similar.
    const SEMANTIC_MIN_SIMILARITY: f32 = 0.3;

    /// Maximum distance threshold for sqlite-vec (L2 distance).
    /// This corresponds roughly to cosine similarity of 0.3 for normalized vectors.
    const SEMANTIC_MAX_DISTANCE: f32 = 1.2;

    fn search_semantic(
        &self,
        query_text: &str,
        limit: usize,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let model = match &self.embedding_model {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        // Generate query embedding
        let query_embedding = model.embed(query_text)?;

        // Use sqlite-vec's efficient vector search instead of loading all chunks
        // This performs the similarity search directly in the database using
        // optimized vector indices, avoiding O(n) memory usage for large codebases.
        let similar_chunks = self
            .store
            .search_similar_chunks(&query_embedding, limit * 2)?;

        if similar_chunks.is_empty() {
            debug!("No similar chunks found via vector search");
            return Ok(Vec::new());
        }

        debug!(
            "Vector search returned {} candidate chunks",
            similar_chunks.len()
        );

        // Filter by distance threshold and collect chunk IDs
        let filtered_results: Vec<(i64, f32)> = similar_chunks
            .into_iter()
            .filter(|(_, distance)| *distance < Self::SEMANTIC_MAX_DISTANCE)
            .collect();

        if filtered_results.is_empty() {
            debug!("No chunks passed distance threshold");
            return Ok(Vec::new());
        }

        // Fetch the actual chunk records by their IDs
        let chunk_ids: Vec<i64> = filtered_results.iter().map(|(id, _)| *id).collect();
        let chunks = self.store.get_chunks_by_ids(&chunk_ids)?;

        // Create a map from chunk_id to distance for scoring
        let distance_map: std::collections::HashMap<i64, f32> =
            filtered_results.into_iter().collect();

        // Convert to SearchResults with proper scoring and filtering
        let results: Vec<SearchResult> = chunks
            .into_iter()
            .filter_map(|chunk| {
                let distance = *distance_map.get(&chunk.id)?;
                // Convert L2 distance to similarity score (0-1 range)
                // Using: similarity = 1 / (1 + distance) for a smooth conversion
                let score = 1.0 / (1.0 + distance);

                // Apply minimum similarity threshold
                if score < Self::SEMANTIC_MIN_SIMILARITY {
                    return None;
                }

                let file_path = self.store.get_chunk_file_path(chunk.file_id).ok()??;

                // Filter by extension
                if let Some(ext) = Path::new(&file_path).extension().and_then(|e| e.to_str())
                    && !options.accepts_extension(ext)
                {
                    return None;
                }

                Some(
                    SearchResult::new(
                        SearchResultKind::SemanticMatch,
                        file_path,
                        chunk.start_line as usize,
                        chunk.end_line as usize,
                        chunk.content.clone(),
                        score,
                    )
                    .with_metadata(SearchResultMetadata {
                        symbol_name: chunk.symbols.first().cloned(),
                        symbol_kind: None,
                        match_type: Some("semantic".to_string()),
                        context: None,
                    }),
                )
            })
            .take(limit)
            .collect();

        debug!("Found {} semantic matches after filtering", results.len());
        Ok(results)
    }

    pub fn find_references(&self, symbol_name: &str, limit: usize) -> Result<SearchResults> {
        info!(symbol = %symbol_name, limit = limit, "Finding references");
        let start = Instant::now();
        let mut results = Vec::new();

        // Find symbol definitions
        let symbols = self.store.find_symbol_by_name(symbol_name)?;

        for symbol in &symbols {
            if let Some(file) = self
                .store
                .get_file_by_path(&self.get_file_path(symbol.file_id)?)?
            {
                let content = self.read_file_lines(
                    &file.path,
                    symbol.start_line as usize,
                    symbol.end_line as usize,
                )?;

                results.push(
                    SearchResult::new(
                        SearchResultKind::Symbol,
                        file.path.clone(),
                        symbol.start_line as usize,
                        symbol.end_line as usize,
                        content,
                        1.0,
                    )
                    .with_metadata(SearchResultMetadata {
                        symbol_name: Some(symbol.name.clone()),
                        symbol_kind: Some(symbol.kind.clone()),
                        match_type: Some("definition".to_string()),
                        context: symbol.signature.clone(),
                    }),
                );
            }
        }

        // Find usages via text search
        let usage_results =
            self.search_text(&Query::new(symbol_name), limit, &SearchOptions::default())?;
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
        debug!(file = %file_path, "Getting dependencies");
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
        info!(symbol = %symbol_name, "Explaining symbol");
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
        let usage_results =
            self.search_text(&Query::new(symbol_name), 100, &SearchOptions::default())?;
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

    fn search_symbols(
        &self,
        query: &Query,
        limit: usize,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();

        for term in query.all_terms() {
            let symbols = self.store.search_symbols(term, limit)?;

            for symbol in symbols {
                // Filter by symbol kind if specified
                if !options.accepts_symbol_kind(&symbol.kind) {
                    continue;
                }

                let file_path = self.get_file_path(symbol.file_id)?;

                // Filter by extension
                if let Some(ext) = Path::new(&file_path).extension().and_then(|e| e.to_str())
                    && !options.accepts_extension(ext)
                {
                    continue;
                }

                let content = symbol
                    .signature
                    .clone()
                    .unwrap_or_else(|| symbol.name.clone());

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
                        symbol_kind: Some(symbol.kind.clone()),
                        match_type: Some("symbol".to_string()),
                        context: symbol.doc_comment,
                    }),
                );
            }
        }

        Ok(results)
    }

    fn search_text(
        &self,
        query: &Query,
        limit: usize,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
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

            // Filter by extension using SearchOptions
            let accepted = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| options.accepts_extension(ext))
                .unwrap_or(false);

            if !accepted {
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
        let searcher = TextSearcher::new(true); // Case insensitive
        let terms = query.all_terms();
        let mut matches = Vec::new();
        let mut seen_lines = std::collections::HashSet::new();

        for term in &terms {
            // Use ripgrep-based search
            if let Ok(results) = searcher.search(content, term) {
                for result in results {
                    // Avoid duplicate lines
                    if seen_lines.insert(result.line_number) {
                        matches.push((result.line_number, result.line_content, result.score));
                    }
                }
            }
        }

        // Sort by score descending
        matches.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

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

        // Safely compute indices, ensuring start_idx <= end_idx <= lines.len()
        let start_idx = start.saturating_sub(1).min(lines.len());
        let end_idx = end.min(lines.len());

        // Handle case where start > end (can happen if file was truncated after indexing)
        if start_idx >= end_idx {
            return Ok(String::new());
        }

        Ok(lines[start_idx..end_idx].join("\n"))
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

    /// Calculate cosine similarity between two vectors.
    /// This function is used only in tests to verify vector operations.
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }

        let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }

        dot_product / (norm_a * norm_b)
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.0001);

        let c = vec![1.0, 0.0, 0.0];
        let d = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&c, &d)).abs() < 0.0001);
    }

    #[test]
    fn test_cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_cosine_similarity_same_direction() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![2.0, 4.0, 6.0];
        // Same direction vectors should have similarity of 1.0
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_cosine_similarity_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_different_lengths() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        // Different lengths should return 0
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_dependency_info_struct() {
        let dep = DependencyInfo {
            target_path: "src/utils.rs".to_string(),
            import_name: Some("utils".to_string()),
            kind: "local".to_string(),
        };

        assert_eq!(dep.target_path, "src/utils.rs");
        assert_eq!(dep.import_name, Some("utils".to_string()));
        assert_eq!(dep.kind, "local");
    }

    #[test]
    fn test_symbol_definition_struct() {
        let def = SymbolDefinition {
            file_path: "src/lib.rs".to_string(),
            kind: "function".to_string(),
            start_line: 10,
            end_line: 20,
            signature: Some("fn process_data()".to_string()),
            doc_comment: Some("/// Process data".to_string()),
        };

        assert_eq!(def.file_path, "src/lib.rs");
        assert_eq!(def.kind, "function");
        assert_eq!(def.start_line, 10);
        assert_eq!(def.end_line, 20);
    }

    #[test]
    fn test_symbol_explanation_not_found() {
        let explanation = SymbolExplanation {
            name: "unknown_symbol".to_string(),
            found: false,
            definitions: Vec::new(),
            usage_count: 0,
            related_symbols: Vec::new(),
        };

        assert!(!explanation.found);
        assert!(explanation.definitions.is_empty());
        assert_eq!(explanation.usage_count, 0);
    }

    #[test]
    fn test_symbol_explanation_found() {
        let explanation = SymbolExplanation {
            name: "process_data".to_string(),
            found: true,
            definitions: vec![SymbolDefinition {
                file_path: "src/lib.rs".to_string(),
                kind: "function".to_string(),
                start_line: 10,
                end_line: 20,
                signature: Some("fn process_data()".to_string()),
                doc_comment: None,
            }],
            usage_count: 5,
            related_symbols: vec!["helper".to_string(), "utils".to_string()],
        };

        assert!(explanation.found);
        assert_eq!(explanation.definitions.len(), 1);
        assert_eq!(explanation.usage_count, 5);
        assert_eq!(explanation.related_symbols.len(), 2);
    }
}
