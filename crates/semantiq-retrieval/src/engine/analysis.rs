//! Code analysis functionality for RetrievalEngine.

use super::RetrievalEngine;
use crate::query::{Query, SearchOptions};
use crate::results::{SearchResult, SearchResultKind, SearchResultMetadata, SearchResults};
use anyhow::Result;
use std::time::Instant;
use tracing::info;

/// Information about a dependency relationship.
#[derive(Debug, Clone)]
pub struct DependencyInfo {
    pub target_path: String,
    pub import_name: Option<String>,
    pub kind: String,
}

/// Explanation of a symbol including definitions and usages.
#[derive(Debug, Clone)]
pub struct SymbolExplanation {
    pub name: String,
    pub found: bool,
    pub definitions: Vec<SymbolDefinition>,
    pub usage_count: usize,
    pub related_symbols: Vec<String>,
}

/// Definition location and metadata for a symbol.
#[derive(Debug, Clone)]
pub struct SymbolDefinition {
    pub file_path: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
}

impl RetrievalEngine {
    /// Find all references to a symbol (definitions + usages).
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

        // Deduplicate: track seen (file_path, start_line) pairs from symbol definitions
        let mut seen = std::collections::HashSet::new();
        for r in &results {
            seen.insert((r.file_path.clone(), r.start_line));
        }

        for mut result in usage_results {
            let key = (result.file_path.clone(), result.start_line);
            if seen.insert(key) {
                result.kind = SearchResultKind::Reference;
                result.metadata.match_type = Some("usage".to_string());
                results.push(result);
            }
        }

        results.truncate(limit);

        let search_time = start.elapsed().as_millis() as u64;
        Ok(SearchResults::new(
            symbol_name.to_string(),
            results,
            search_time,
        ))
    }

    /// Get dependencies for a file (what it imports).
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

    /// Get dependents for a file (what imports it).
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

    /// Get detailed explanation of a symbol.
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

        // Limit definitions processed to avoid excessive DB queries (N+1 pattern).
        // For symbols defined in many files, the first 20 are sufficient.
        let max_definitions = 20;
        let mut seen_file_ids = std::collections::HashSet::new();

        for symbol in symbols.iter().take(max_definitions) {
            let file_path = self.get_file_path(symbol.file_id)?;

            definitions.push(SymbolDefinition {
                file_path: file_path.clone(),
                kind: symbol.kind.clone(),
                start_line: symbol.start_line as usize,
                end_line: symbol.end_line as usize,
                signature: symbol.signature.clone(),
                doc_comment: symbol.doc_comment.clone(),
            });

            // Find related symbols in the same file (only query each file once)
            if seen_file_ids.insert(symbol.file_id) {
                let file_symbols = self.store.get_symbols_by_file(symbol.file_id)?;
                for fs in file_symbols {
                    if fs.name != symbol_name {
                        related_symbols.insert(fs.name);
                    }
                }
            }
        }

        // Count usages via FTS5 (much faster than reading files from disk)
        // Falls back to text search if FTS5 returns no results
        let usage_count = match self.store.search_symbols(symbol_name, 100) {
            Ok(fts_results) => {
                // FTS5 returns symbol definitions; add a conservative estimate
                // for text usages beyond definitions
                let definition_count = definitions.len();
                fts_results.len().saturating_sub(definition_count)
            }
            Err(_) => {
                // Fallback: use text search (slower but more accurate)
                let usage_results =
                    self.search_text(&Query::new(symbol_name), 100, &SearchOptions::default())?;
                usage_results.len()
            }
        };

        Ok(SymbolExplanation {
            name: symbol_name.to_string(),
            found: true,
            definitions,
            usage_count,
            related_symbols: related_symbols.into_iter().collect(),
        })
    }
}
