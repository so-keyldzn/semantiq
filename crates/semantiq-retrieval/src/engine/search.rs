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
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Maximum limit for search results to prevent excessive memory usage.
const MAX_SEARCH_LIMIT: usize = 1000;

/// Per-strategy weights applied to each strategy's locally-normalized score
/// before the global merge.
///
/// Rationale: the three strategies produce scores on incompatible scales
/// (semantic = `1/(1+L2_distance)`, symbol = a hand-tuned `[0,1]` heuristic,
/// text = ripgrep line heuristic). Sorting them together raw lets, e.g., a
/// borderline 0.5 semantic hit outrank a 0.49 exact-symbol hit purely because
/// of scale, not relevance. We rescale each strategy's results to `[0,1]`
/// (min-max within the strategy) and then apply a fixed weight so that, all
/// else equal, an exact symbol match outranks a fuzzy semantic match which
/// outranks a plain text/grep hit. Min-max is intentionally conservative: it
/// preserves the *intra-strategy* ordering and only makes the *inter-strategy*
/// comparison meaningful. A single dominant result in a strategy keeps its
/// weight (we map it to 1.0 rather than 0.0).
pub(crate) const WEIGHT_SEMANTIC: f32 = 0.95;
pub(crate) const WEIGHT_SYMBOL: f32 = 1.0;
pub(crate) const WEIGHT_TEXT: f32 = 0.75;

/// Min-max normalize the `score` of a strategy's results in place, then scale
/// by `weight`. Preserves the relative ordering inside the strategy while
/// making cross-strategy comparison meaningful at merge time.
pub(crate) fn normalize_and_weight(results: &mut [SearchResult], weight: f32) {
    if results.is_empty() {
        return;
    }
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for r in results.iter() {
        min = min.min(r.score);
        max = max.max(r.score);
    }
    let span = max - min;
    for r in results.iter_mut() {
        // When all scores are equal (span == 0), keep them at full strength
        // rather than collapsing to 0 — a single strong hit shouldn't be
        // penalized for lacking a spread to normalize against.
        let normalized = if span > f32::EPSILON {
            (r.score - min) / span
        } else {
            1.0
        };
        r.score = normalized * weight;
    }
}

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

        // 1. Semantic search (vector similarity) - highest priority.
        //
        // Skip entirely when the embedding model is a stub: a stub returns a
        // zero vector for every query, so every chunk has L2 distance 0 →
        // score 1.0. That would flood the merge with arbitrary, off-topic
        // chunks ranked above genuine symbol/text matches. Only run semantic
        // search when a real model is present.
        let semantic_enabled = self
            .embedding_model
            .as_ref()
            .map(|m| !m.is_stub())
            .unwrap_or(false);
        if semantic_enabled {
            let mut semantic_results = self.search_semantic(query_text, safe_limit, &opts)?;
            normalize_and_weight(&mut semantic_results, WEIGHT_SEMANTIC);
            all_results.extend(semantic_results);
        }

        // 2. Symbol search (FTS) - prioritize symbol matches
        let mut symbol_results = self.search_symbols(&query, safe_limit, &opts)?;
        normalize_and_weight(&mut symbol_results, WEIGHT_SYMBOL);
        all_results.extend(symbol_results);

        // 3. Text search (grep-like) - only if we need more results
        if all_results.len() < safe_limit {
            let mut text_results =
                self.search_text(&query, safe_limit - all_results.len(), &opts)?;
            normalize_and_weight(&mut text_results, WEIGHT_TEXT);
            all_results.extend(text_results);
        }

        // Sort by score (highest first). Scores are now comparable across
        // strategies because each strategy was min-max normalized and weighted
        // above before being merged here.
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

        // Resolve the language for each candidate chunk *once* and reuse the
        // result for both calibration recording and dominant-language
        // detection. Previously each helper scanned the chunks independently,
        // issuing a `get_chunk_language` query per chunk twice over.
        let language_by_chunk = self.languages_for_chunks(&similar_chunks);

        // Collect distance observations for ML calibration
        self.collect_distance_observations(query_text, &similar_chunks, &language_by_chunk);

        // Detect dominant language from results for adaptive thresholds
        let dominant_language = self.detect_dominant_language(&similar_chunks, &language_by_chunk);

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

        // Pre-fetch symbols for all the unique files we touch so we can attach
        // a `kind` to each result without a per-chunk query.
        let unique_file_ids: std::collections::HashSet<i64> =
            chunks.iter().map(|c| c.file_id).collect();
        let mut kinds_by_file: std::collections::HashMap<
            i64,
            std::collections::HashMap<String, String>,
        > = std::collections::HashMap::new();
        for fid in unique_file_ids {
            match self.store.get_symbols_by_file(fid) {
                Ok(syms) => {
                    let mut map = std::collections::HashMap::new();
                    for s in syms {
                        // Last write wins on duplicate names; good enough for
                        // labelling and avoids holding a Vec of kinds per name.
                        map.insert(s.name, s.kind);
                    }
                    kinds_by_file.insert(fid, map);
                }
                Err(e) => {
                    // Non-fatal: results just lose their `symbol_kind` label
                    // (the old behavior). But surface it — a recurring DB
                    // error here usually means the connection is wedged.
                    warn!(file_id = fid, error = %e, "failed to fetch symbols for kind lookup");
                }
            }
        }

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

                let symbol_name = chunk.symbols.first().cloned();
                let symbol_kind = symbol_name
                    .as_ref()
                    .and_then(|n| kinds_by_file.get(&chunk.file_id).and_then(|m| m.get(n)))
                    .cloned();

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
                        symbol_name,
                        symbol_kind,
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

    /// Resolve the language of each unique chunk in `results` with a single
    /// lookup per chunk.
    ///
    /// Both `collect_distance_observations` and `detect_dominant_language`
    /// need the per-chunk language; building this map once and sharing it
    /// avoids querying every chunk twice (once per helper) per search.
    pub(crate) fn languages_for_chunks(
        &self,
        results: &[(i64, f32)],
    ) -> std::collections::HashMap<i64, Option<String>> {
        let mut map: std::collections::HashMap<i64, Option<String>> =
            std::collections::HashMap::with_capacity(results.len());
        for (chunk_id, _) in results {
            map.entry(*chunk_id)
                .or_insert_with(|| self.store.get_chunk_language(*chunk_id).ok().flatten());
        }
        map
    }

    /// Collect distance observations for ML calibration.
    ///
    /// `language_by_chunk` is the shared, pre-resolved language map (see
    /// [`languages_for_chunks`]) so no DB lookups happen here.
    pub(crate) fn collect_distance_observations(
        &self,
        query: &str,
        results: &[(i64, f32)],
        language_by_chunk: &std::collections::HashMap<i64, Option<String>>,
    ) {
        let collector = match &self.distance_collector {
            Some(c) => c,
            None => {
                debug!("Distance collector not enabled");
                return;
            }
        };

        let recorded = collector.record(query, results, |chunk_id| {
            language_by_chunk.get(&chunk_id).cloned().flatten()
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

    /// Detect the dominant programming language from search results using the
    /// shared, pre-resolved language map (see [`languages_for_chunks`]).
    pub(crate) fn detect_dominant_language(
        &self,
        results: &[(i64, f32)],
        language_by_chunk: &std::collections::HashMap<i64, Option<String>>,
    ) -> Option<String> {
        if results.is_empty() {
            return None;
        }

        let mut language_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for (chunk_id, _) in results.iter().take(5) {
            if let Some(Some(lang)) = language_by_chunk.get(chunk_id) {
                *language_counts.entry(lang.clone()).or_insert(0) += 1;
            }
        }

        language_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(lang, _)| lang)
    }

    /// Score penalty applied to symbol matches found via an *expanded* term
    /// (a case variant) rather than the original query term. Keeps results
    /// matched by the user's literal input ranked above synthetic variants.
    const SYMBOL_VARIANT_PENALTY: f32 = 0.9;

    /// Search symbols using FTS5 full-text search.
    pub(crate) fn search_symbols(
        &self,
        query: &Query,
        limit: usize,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();

        // Dedup the candidate symbols across all (original + expanded) terms so
        // the same symbol matched by multiple variants is only scored/emitted
        // once, keyed by "file:start:end". Without this, an N-variant query
        // amplified results N-fold before the outer search() dedup ran.
        let mut seen_symbols = std::collections::HashSet::new();

        // Memoize file-path lookups: many symbols share a file, and previously
        // every symbol triggered its own `get_file_path` query (N+1). One
        // lookup per distinct file_id now.
        let mut file_path_cache: std::collections::HashMap<i64, Option<String>> =
            std::collections::HashMap::new();

        // The first entry of `all_terms()` is the original query text; the rest
        // are expanded case variants. Track which terms we've already queried so
        // we never re-run FTS5 for a duplicate variant string.
        let mut queried_terms = std::collections::HashSet::new();
        let terms = query.all_terms();
        let original_term = terms.first().copied();

        for term in &terms {
            if !queried_terms.insert(term.to_lowercase()) {
                // Identical variant (case-insensitively) already queried.
                continue;
            }

            let is_original = Some(*term) == original_term;
            let symbols = self.store.search_symbols(term, limit)?;

            for symbol in symbols {
                if !options.accepts_symbol_kind(&symbol.kind) {
                    continue;
                }

                let dedup_key = format!(
                    "{}:{}:{}",
                    symbol.file_id, symbol.start_line, symbol.end_line
                );
                if !seen_symbols.insert(dedup_key) {
                    continue;
                }

                let file_path = match file_path_cache.entry(symbol.file_id) {
                    std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let resolved = self.store.get_file_path_by_id(symbol.file_id)?;
                        e.insert(resolved).clone()
                    }
                };
                let file_path = match file_path {
                    Some(p) => p,
                    None => continue,
                };

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

                // Penalize matches that came from an expanded variant so the
                // user's literal term wins ties against synthetic variants.
                if !is_original {
                    score *= Self::SYMBOL_VARIANT_PENALTY;
                }

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

        // Compile the matchers for every (deduplicated) query term exactly
        // once for the whole request instead of recompiling them per file.
        let searcher = TextSearcher::new(true);
        let mut compiled = Vec::new();
        let mut seen_terms = std::collections::HashSet::new();
        for term in query.all_terms() {
            if seen_terms.insert(term.to_lowercase())
                && let Ok(m) = searcher.compile(term)
            {
                compiled.push(m);
            }
        }

        for path in file_paths.iter() {
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

            // Bound memory: skip files larger than MAX_FILE_SIZE rather than
            // slurping an arbitrarily large file fully into RAM. Indexing
            // already skips these, so they carry no symbols/chunks anyway.
            if let Ok(meta) = fs::metadata(path)
                && meta.len() > semantiq_index::MAX_FILE_SIZE
            {
                continue;
            }

            if let Ok(content) = fs::read_to_string(path) {
                let matches = Self::find_text_matches_compiled(&searcher, &content, &compiled);
                if matches.is_empty() {
                    continue;
                }
                // Hoist out of the per-match loop: the path doesn't change
                // between matches, but `to_relative_string` is non-trivial
                // (canonicalize fallback). Saves one call per extra hit.
                let rel_path = semantiq_index::paths::to_relative_string(path, root);

                for (line_num, line_content, score) in matches {
                    results.push(SearchResult::new(
                        SearchResultKind::TextMatch,
                        rel_path.clone(),
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
    ///
    /// Returns an `Arc` handle so the caller shares the cached `Vec` instead of
    /// deep-cloning it on every `search_text()` call.
    fn get_cached_file_list(&self, root: &Path) -> Result<Arc<Vec<PathBuf>>> {
        use super::{FILE_LIST_CACHE_TTL_SECS, FileListCache};
        use std::time::Duration;

        let mut cache = self
            .file_list_cache
            .lock()
            .map_err(|e| anyhow::anyhow!("File list cache lock poisoned: {}", e))?;

        if let Some(ref cached) = *cache
            && cached.created_at.elapsed() < Duration::from_secs(FILE_LIST_CACHE_TTL_SECS)
        {
            return Ok(Arc::clone(&cached.paths));
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

        let paths: Arc<Vec<PathBuf>> = Arc::new(
            walker
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .map(|e| e.into_path())
                .collect(),
        );

        *cache = Some(FileListCache {
            paths: Arc::clone(&paths),
            created_at: std::time::Instant::now(),
        });

        Ok(paths)
    }

    /// Find text matches in content using pre-compiled matchers.
    ///
    /// Dedupes by line number across all matchers and returns matches sorted
    /// by score (highest first).
    pub(crate) fn find_text_matches_compiled(
        searcher: &TextSearcher,
        content: &str,
        compiled: &[crate::text_searcher::CompiledMatcher],
    ) -> Vec<(usize, String, f32)> {
        let mut matches = Vec::new();
        let mut seen_lines = std::collections::HashSet::new();

        for matcher in compiled {
            if let Ok(results) = searcher.search_compiled(content, matcher) {
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
