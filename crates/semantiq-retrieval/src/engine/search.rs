//! Search functionality for RetrievalEngine.

use super::RetrievalEngine;
use crate::query::{Query, SearchOptions};
use crate::results::{SearchResult, SearchResultKind, SearchResultMetadata, SearchResults};
use crate::text_searcher::{CompiledMatcher, HashLine, TextSearcher, looks_generated};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Maximum limit for search results to prevent excessive memory usage.
const MAX_SEARCH_LIMIT: usize = 1000;

/// Largest KNN candidate pool the semantic strategy grows to when a
/// `file_type` / `symbol_kind` filter leaves it short of `limit` hits.
const MAX_SEMANTIC_CANDIDATES: usize = 2048;

/// Chunks fetched per page of the text strategy.
const TEXT_PAGE: usize = 64;

/// Per-strategy weights applied to each strategy's raw score before the
/// global merge.
///
/// Rationale: the three strategies produce scores in `[0, 1]` with different
/// meanings (semantic = `1/(1+L2_distance)`, symbol = a hand-tuned match
/// heuristic, text = ripgrep line heuristic). Sorting them together raw lets,
/// e.g., a borderline 0.5 semantic hit outrank a 0.49 exact-symbol hit purely
/// because of scale, not relevance. A fixed weight per strategy makes, all
/// else equal, an exact symbol match outrank a fuzzy semantic match which
/// outranks a plain text/grep hit.
///
/// The raw scores are kept otherwise: an earlier min-max rescaling within
/// each strategy sent the weakest hit of a strategy to 0 (a prefix match
/// ranked below every grep line) and inflated a lone weak hit to full
/// strength (a single 0.45 substring match outranked the best semantic
/// chunk).
pub(crate) const WEIGHT_SEMANTIC: f32 = 0.95;
pub(crate) const WEIGHT_SYMBOL: f32 = 1.0;
pub(crate) const WEIGHT_TEXT: f32 = 0.75;

/// Scale the `score` of a strategy's results by `weight`, in place. Every
/// strategy already scores in `[0, 1]`, so a hit keeps its own strength: a
/// single hit stays as meaningful as its raw score and no real hit is sent
/// to 0.
pub(crate) fn weight_strategy(results: &mut [SearchResult], weight: f32) {
    for r in results.iter_mut() {
        r.score *= weight;
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

        // `min_score` is an absolute floor on each strategy's RAW score,
        // applied before the strategy weight.
        let min_score = opts.effective_min_score();
        let above_floor = |results: &mut Vec<SearchResult>| {
            results.retain(|r| r.score >= min_score);
        };

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
            above_floor(&mut semantic_results);
            weight_strategy(&mut semantic_results, WEIGHT_SEMANTIC);
            all_results.extend(semantic_results);
        }

        // 2. Symbol search (FTS) - prioritize symbol matches
        let mut symbol_results = self.search_symbols(&query, safe_limit, &opts)?;
        above_floor(&mut symbol_results);
        weight_strategy(&mut symbol_results, WEIGHT_SYMBOL);
        all_results.extend(symbol_results);

        // 3. Text search (grep-like) - only if we need more results. A text
        // hit is a line, not a symbol: when the caller asked for symbol kinds
        // it cannot satisfy the filter (#93).
        if all_results.len() < safe_limit && opts.symbol_kinds.is_none() {
            let mut text_results =
                self.search_text(&query, safe_limit - all_results.len(), &opts)?;
            above_floor(&mut text_results);
            weight_strategy(&mut text_results, WEIGHT_TEXT);
            all_results.extend(text_results);
        }

        // Sort by score (highest first). Scores are comparable across
        // strategies because each strategy was weighted above.
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

        // Generate query embedding (query-side encoding, see EmbeddingModel::embed_query)
        let query_embedding = model.embed_query(query_text)?;

        // Does `hit` pass the file-type and symbol-kind filters? A chunk
        // satisfies a kind filter when one of its symbols has that kind.
        let kind_filtered = options.symbol_kinds.is_some();
        let accepts = |hit: &semantiq_index::ChunkHit,
                       kinds_by_file: &HashMap<i64, HashMap<String, String>>|
         -> bool {
            if let Some(ext) = Path::new(&hit.file_path)
                .extension()
                .and_then(|e| e.to_str())
                && !options.accepts_extension(ext)
            {
                return false;
            }
            if !kind_filtered {
                return true;
            }
            let kinds = kinds_by_file.get(&hit.file_id);
            hit.symbols.iter().any(|name| {
                kinds
                    .and_then(|m| m.get(name))
                    .is_some_and(|kind| options.accepts_symbol_kind(kind))
            })
        };

        // Use sqlite-vec's efficient vector search. With a filter set, the
        // candidate pool grows until `limit` candidates pass it or the index
        // runs out (#134): the top 2 × limit of a mostly-Rust index may hold
        // no Python chunk at all.
        let filtered = kind_filtered || options.file_types.is_some();
        let mut fetch = limit * 2;
        let (similar_chunks, hits, kinds_by_file) = loop {
            let similar_chunks = self.store.search_similar_chunks(&query_embedding, fetch)?;

            // Every candidate with its file's path and language, in one
            // query: the language feeds calibration and the dominant-language
            // thresholds, the path the results. One lookup per chunk here
            // (then per file, then per file's symbols) made a `limit 100`
            // search ~500 round-trips on the shared connection.
            let chunk_ids: Vec<i64> = similar_chunks.iter().map(|(id, _)| *id).collect();
            let hits = self.store.get_chunk_hits(&chunk_ids)?;

            // Symbol kinds of every file touched, in one query, to label each
            // result with the kind of its first symbol and apply the kind
            // filter.
            let mut file_ids: Vec<i64> = hits.iter().map(|hit| hit.file_id).collect();
            file_ids.sort_unstable();
            file_ids.dedup();
            let kinds_by_file = match self.store.get_symbol_kinds_by_files(&file_ids) {
                Ok(kinds) => kinds,
                Err(e) => {
                    // Non-fatal: results just lose their `symbol_kind` label.
                    // But surface it: a recurring DB error here usually means
                    // the connection is wedged.
                    warn!(error = %e, "failed to fetch symbols for kind lookup");
                    Default::default()
                }
            };

            let exhausted = similar_chunks.len() < fetch || fetch >= MAX_SEMANTIC_CANDIDATES;
            if !filtered
                || exhausted
                || hits
                    .iter()
                    .filter(|hit| accepts(hit, &kinds_by_file))
                    .count()
                    >= limit
            {
                break (similar_chunks, hits, kinds_by_file);
            }
            fetch = (fetch * 4).min(MAX_SEMANTIC_CANDIDATES);
        };

        if similar_chunks.is_empty() {
            debug!("No similar chunks found via vector search");
            return Ok(Vec::new());
        }

        debug!(
            "Vector search returned {} candidate chunks",
            similar_chunks.len()
        );

        let language_by_chunk: HashMap<i64, Option<String>> = hits
            .iter()
            .map(|hit| (hit.id, hit.language.clone()))
            .collect();

        // Collect distance observations for ML calibration, from the same
        // 2 × limit nearest candidates whatever the pool grew to.
        let observed = &similar_chunks[..similar_chunks.len().min(limit * 2)];
        self.collect_distance_observations(query_text, observed, &language_by_chunk);

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
        let distance_map: HashMap<i64, f32> = similar_chunks
            .into_iter()
            .filter(|(_, distance)| *distance < max_distance)
            .collect();
        let hits: Vec<_> = hits
            .into_iter()
            .filter(|hit| distance_map.contains_key(&hit.id))
            .collect();

        if hits.is_empty() {
            debug!("No chunks passed distance threshold");
            return Ok(Vec::new());
        }

        // Convert to SearchResults
        let results: Vec<SearchResult> = hits
            .into_iter()
            .filter_map(|chunk| {
                let distance = *distance_map.get(&chunk.id)?;
                let score = 1.0 / (1.0 + distance);

                if score < min_similarity || !accepts(&chunk, &kinds_by_file) {
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
                        chunk.file_path,
                        chunk.start_line as usize,
                        chunk.end_line as usize,
                        chunk.content,
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

    /// Collect distance observations for ML calibration.
    ///
    /// `language_by_chunk` is the language of each candidate chunk, resolved
    /// with the chunks themselves, so no DB lookups happen here.
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
    /// pre-resolved language map of the candidate chunks.
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

    /// Largest kind boost (`function` / `method`) and largest length boost
    /// (a one-character name) a symbol score can get. Scores are divided by
    /// their product so the boosts rank symbols inside `[0, 1]`: clamping the
    /// boosted score to 1.0 instead tied an exact function match (1.0 × 1.15)
    /// with a boosted prefix match (0.85 × 1.15 × 1.1).
    const MAX_SYMBOL_BOOST: f32 = 1.15 * (1.0 + 1.0 / 6.0);

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
        let mut seen_symbols = HashSet::new();

        // The first entry of `all_terms()` is the original query text; the rest
        // are expanded case variants. Track which terms we've already queried so
        // we never re-run FTS5 for a duplicate variant string.
        let mut queried_terms = HashSet::new();
        let terms = query.all_terms();
        let original_term = terms.first().copied();

        // Kind and extension filters apply in SQL, before the FTS `LIMIT`
        // (#134); `import` rows are left out unless asked for (#77).
        let kinds = options.symbol_kinds.as_deref();
        let files = options.extension_filter();

        for term in &terms {
            if term.trim().is_empty() || !queried_terms.insert(term.to_lowercase()) {
                // Empty, or an identical variant (case-insensitively) already queried.
                continue;
            }

            let is_original = Some(*term) == original_term;
            let symbols = self
                .store
                .search_symbols_filtered(term, kinds, &files, limit)?;

            for (symbol, file_path) in symbols {
                let dedup_key = format!(
                    "{}:{}:{}",
                    symbol.file_id, symbol.start_line, symbol.end_line
                );
                if !seen_symbols.insert(dedup_key) {
                    continue;
                }

                let content = symbol
                    .signature
                    .clone()
                    .unwrap_or_else(|| symbol.name.clone());

                // Match tier in [0, 1]: exact > prefix > substring > FTS only.
                let name_lower = symbol.name.to_lowercase();
                let term_lower = term.to_lowercase();

                let mut score: f32 = if name_lower == term_lower {
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

                // Back into [0, 1] without clamping (see MAX_SYMBOL_BOOST): a
                // name matching the term exactly is never longer than a name
                // it is a prefix of, so an exact match keeps the edge.
                score /= Self::MAX_SYMBOL_BOOST;

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

    /// Search text content using grep-like matching, answered from the
    /// index: the trigram FTS index over the chunks selects the chunks
    /// holding a term (ranked, paged), and each term's regex locates the
    /// lines in them. No file of the project is read (#160).
    pub(crate) fn search_text(
        &self,
        query: &Query,
        limit: usize,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        // Compile the matchers for every (deduplicated) query term exactly
        // once for the whole request instead of recompiling them per chunk.
        let searcher = TextSearcher::new(true);
        let mut compiled = Vec::new();
        let mut seen_terms = HashSet::new();
        for term in query.all_terms() {
            if !term.trim().is_empty()
                && seen_terms.insert(term.to_lowercase())
                && let Ok(m) = searcher.compile(term)
            {
                compiled.push(m);
            }
        }
        self.search_chunks_with(&searcher, &compiled, limit, &options.extension_filter())
    }

    /// Lines where `name` occurs as a whole identifier, case-sensitive: the
    /// text approximation of the `refs` table for names it does not know
    /// (keys of data files). No case variants, no substrings: `id` does not
    /// match `uuid`, `width` or `ID`. By file path and line.
    pub(crate) fn search_identifier_text(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if name.trim().is_empty() {
            return Ok(Vec::new());
        }
        let searcher = TextSearcher::new(false);
        let compiled = [searcher.compile_word(name)?];
        let mut results = self.search_chunks_with(
            &searcher,
            &compiled,
            limit,
            &SearchOptions::default().extension_filter(),
        )?;
        results.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.start_line.cmp(&b.start_line))
        });
        Ok(results)
    }

    /// Collect up to `limit` lines matched by `compiled` in the indexed
    /// chunks of the files accepted by `files`. Each matcher's literal text
    /// selects its candidate chunks through the trigram index (a
    /// case-insensitive superset of the regex matches), pages of
    /// [`TEXT_PAGE`] chunks, best FTS rank first, until `limit` lines are
    /// found or the candidates run out. Lines are deduplicated across
    /// matchers and across overlapping chunks.
    fn search_chunks_with(
        &self,
        searcher: &TextSearcher,
        compiled: &[CompiledMatcher],
        limit: usize,
        files: &semantiq_index::ExtensionFilter,
    ) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();
        let mut seen_lines: HashSet<(String, usize)> = HashSet::new();

        'matchers: for matcher in compiled {
            let mut offset = 0;
            loop {
                let hits =
                    self.store
                        .search_chunk_text(matcher.pattern(), files, TEXT_PAGE, offset)?;
                for hit in &hits {
                    // Minified / generated content would return its whole
                    // content as one hit.
                    if looks_generated(&hit.content) {
                        continue;
                    }
                    let rule = HashLine::for_language(hit.language.as_deref());
                    let Ok(matches) = searcher.search_compiled(&hit.content, matcher, rule) else {
                        continue;
                    };
                    let first_line = hit.start_line.max(1) as usize;
                    for m in matches {
                        let line = first_line + m.line_number.saturating_sub(1);
                        if !seen_lines.insert((hit.file_path.clone(), line)) {
                            continue;
                        }
                        results.push(SearchResult::new(
                            SearchResultKind::TextMatch,
                            hit.file_path.clone(),
                            line,
                            line,
                            m.line_content,
                            m.score,
                        ));
                        if results.len() >= limit {
                            break 'matchers;
                        }
                    }
                }
                if hits.len() < TEXT_PAGE {
                    break;
                }
                offset += TEXT_PAGE;
            }
        }

        Ok(results)
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
