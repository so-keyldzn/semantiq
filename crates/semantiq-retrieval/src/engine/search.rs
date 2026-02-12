//! Search functionality for RetrievalEngine.

use super::RetrievalEngine;
use crate::query::{Query, SearchOptions};
use crate::results::{SearchResult, SearchResultKind, SearchResultMetadata, SearchResults};
use crate::text_searcher::TextSearcher;
use anyhow::Result;
use ignore::WalkBuilder;
use semantiq_index::should_exclude_entry;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{debug, info, warn};

/// Maximum limit for search results to prevent excessive memory usage.
const MAX_SEARCH_LIMIT: usize = 1000;

impl RetrievalEngine {
    /// Perform a multi-strategy search combining semantic, symbol, and text search.
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
        let safe_limit = limit.min(MAX_SEARCH_LIMIT);
        if limit > MAX_SEARCH_LIMIT {
            warn!(
                requested = limit,
                max = MAX_SEARCH_LIMIT,
                "Requested limit exceeds maximum, capping to {}",
                MAX_SEARCH_LIMIT
            );
        }

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

        // Sort by score (highest first)
        all_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Remove duplicates based on file_path + start_line + end_line
        // Using start_line + end_line is more reliable than content.len() which
        // could collide for different content of the same length
        let mut seen = std::collections::HashSet::new();
        all_results.retain(|r| {
            let key = format!("{}:{}:{}", r.file_path, r.start_line, r.end_line);
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

    /// Perform semantic (vector similarity) search.
    pub(crate) fn search_semantic(
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

        // Use sqlite-vec's efficient vector search
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

        // Collect distance observations for ML calibration
        self.collect_distance_observations(query_text, &similar_chunks);

        // Detect dominant language from results for adaptive thresholds
        let dominant_language = self.detect_dominant_language(&similar_chunks);

        // Get adaptive thresholds
        let (max_distance, min_similarity) = self.get_thresholds(dominant_language.as_deref());

        debug!(
            language = ?dominant_language,
            max_distance = max_distance,
            min_similarity = min_similarity,
            "Using thresholds"
        );

        // Filter by distance threshold
        let filtered_results: Vec<(i64, f32)> = similar_chunks
            .into_iter()
            .filter(|(_, distance)| *distance < max_distance)
            .collect();

        if filtered_results.is_empty() {
            debug!("No chunks passed distance threshold");
            return Ok(Vec::new());
        }

        // Fetch the actual chunk records
        let chunk_ids: Vec<i64> = filtered_results.iter().map(|(id, _)| *id).collect();
        let chunks = self.store.get_chunks_by_ids(&chunk_ids)?;

        // Create a map from chunk_id to distance for scoring
        let distance_map: std::collections::HashMap<i64, f32> =
            filtered_results.into_iter().collect();

        // Convert to SearchResults
        let results: Vec<SearchResult> = chunks
            .into_iter()
            .filter_map(|chunk| {
                let distance = *distance_map.get(&chunk.id)?;
                let score = 1.0 / (1.0 + distance);

                if score < min_similarity {
                    return None;
                }

                let file_path = self.store.get_chunk_file_path(chunk.file_id).ok()??;

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

        // Flush observations if buffer is full
        self.maybe_flush_observations();

        debug!("Found {} semantic matches after filtering", results.len());
        Ok(results)
    }

    /// Collect distance observations for ML calibration.
    pub(crate) fn collect_distance_observations(&self, query: &str, results: &[(i64, f32)]) {
        let collector = match &self.distance_collector {
            Some(c) => c,
            None => {
                debug!("Distance collector not enabled");
                return;
            }
        };

        let store = &self.store;
        let recorded = collector.record(query, results, |chunk_id| {
            store.get_chunk_language(chunk_id).ok().flatten()
        });

        if recorded {
            debug!(
                query = query,
                results = results.len(),
                buffer_len = collector.buffer_len(),
                "Recorded distance observations"
            );
        }
    }

    /// Detect the dominant programming language from search results.
    pub(crate) fn detect_dominant_language(&self, results: &[(i64, f32)]) -> Option<String> {
        if results.is_empty() {
            return None;
        }

        let mut language_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for (chunk_id, _) in results.iter().take(5) {
            if let Ok(Some(lang)) = self.store.get_chunk_language(*chunk_id) {
                *language_counts.entry(lang).or_insert(0) += 1;
            }
        }

        language_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(lang, _)| lang)
    }

    /// Search symbols using FTS5 full-text search.
    pub(crate) fn search_symbols(
        &self,
        query: &Query,
        limit: usize,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();

        for term in query.all_terms() {
            let symbols = self.store.search_symbols(term, limit)?;

            for symbol in symbols {
                if !options.accepts_symbol_kind(&symbol.kind) {
                    continue;
                }

                let file_path = self.get_file_path(symbol.file_id)?;

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
                    1.0
                } else if name_lower.starts_with(&term_lower) {
                    0.85
                } else if name_lower.contains(&term_lower) {
                    0.7
                } else {
                    0.5
                };

                // Boost score based on symbol kind
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

                // Slight boost for shorter names
                let length_factor = 1.0 + (1.0 / (symbol.name.len() as f32 + 5.0));
                score *= length_factor;

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

    /// Search text content using grep-like matching.
    ///
    /// Uses a cached file list (with TTL) to avoid re-walking the directory
    /// tree on every call within the same session.
    pub(crate) fn search_text(
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

        let file_paths = self.get_cached_file_list(root)?;

        for path in &file_paths {
            if results.len() >= limit {
                break;
            }

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

    /// Get the cached file list, rebuilding it if the cache has expired.
    fn get_cached_file_list(&self, root: &Path) -> Result<Vec<PathBuf>> {
        use super::{FILE_LIST_CACHE_TTL_SECS, FileListCache};
        use std::time::Duration;

        let mut cache = self
            .file_list_cache
            .lock()
            .map_err(|e| anyhow::anyhow!("File list cache lock poisoned: {}", e))?;

        if let Some(ref cached) = *cache
            && cached.created_at.elapsed() < Duration::from_secs(FILE_LIST_CACHE_TTL_SECS)
        {
            return Ok(cached.paths.clone());
        }

        // Rebuild the file list
        let walker = WalkBuilder::new(root)
            .hidden(true)
            .git_ignore(true)
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                !should_exclude_entry(&name)
            })
            .build();

        let paths: Vec<PathBuf> = walker
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| e.into_path())
            .collect();

        *cache = Some(FileListCache {
            paths: paths.clone(),
            created_at: std::time::Instant::now(),
        });

        Ok(paths)
    }

    /// Find text matches in content.
    pub(crate) fn find_text_matches(
        &self,
        content: &str,
        query: &Query,
    ) -> Vec<(usize, String, f32)> {
        let searcher = TextSearcher::new(true);
        let terms = query.all_terms();
        let mut matches = Vec::new();
        let mut seen_lines = std::collections::HashSet::new();

        for term in &terms {
            if let Ok(results) = searcher.search(content, term) {
                for result in results {
                    if seen_lines.insert(result.line_number) {
                        matches.push((result.line_number, result.line_content, result.score));
                    }
                }
            }
        }

        matches.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        matches
    }

    /// Get file path from file ID.
    pub(crate) fn get_file_path(&self, file_id: i64) -> Result<String> {
        self.store
            .get_file_path_by_id(file_id)?
            .ok_or_else(|| anyhow::anyhow!("File not found with id: {}", file_id))
    }

    /// Read specific lines from a file.
    ///
    /// Validates that the resolved path stays within `root_path` to prevent
    /// path traversal attacks via `..` sequences.
    pub(crate) fn read_file_lines(
        &self,
        file_path: &str,
        start: usize,
        end: usize,
    ) -> Result<String> {
        let root = Path::new(&self.root_path);
        let full_path = root.join(file_path);

        // Canonicalize to resolve symlinks and .. components, then verify
        // the resolved path is still within the project root.
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let canonical_path = full_path
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("Cannot resolve file path: {}", e))?;

        if !canonical_path.starts_with(&canonical_root) {
            return Err(anyhow::anyhow!(
                "Access denied: path is outside the project root"
            ));
        }

        let content = fs::read_to_string(&canonical_path)?;
        let lines: Vec<&str> = content.lines().collect();

        let start_idx = start.saturating_sub(1).min(lines.len());
        let end_idx = end.min(lines.len());

        if start_idx >= end_idx {
            return Ok(String::new());
        }

        Ok(lines[start_idx..end_idx].join("\n"))
    }
}
