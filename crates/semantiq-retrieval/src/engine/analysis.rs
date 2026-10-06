//! Code analysis functionality for RetrievalEngine.

use super::RetrievalEngine;
use crate::query::{Query, SearchOptions};
use crate::results::{SearchResult, SearchResultKind, SearchResultMetadata, SearchResults};
use anyhow::Result;
use std::collections::HashMap;
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
    ///
    /// Definitions come from the symbol table. Usages come from the AST
    /// `refs` table, so comments, strings and longer names containing the
    /// symbol are never reported. Names absent from `refs` (e.g. keys in data
    /// files, which have no AST references) fall back to text search, with
    /// `match_type = "text"`.
    pub fn find_references(&self, symbol_name: &str, limit: usize) -> Result<SearchResults> {
        info!(symbol = %symbol_name, limit = limit, "Finding references");
        let start = Instant::now();
        let mut results = Vec::new();

        // Find symbol definitions
        let symbols = self.store.find_symbol_by_name(symbol_name)?;
        // (path, start_line, end_line) of each definition, to drop the
        // definition-site occurrences already covered by the symbol table.
        let mut definition_spans = Vec::new();

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

                definition_spans.push((file.path.clone(), symbol.start_line, symbol.end_line));
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

        let references = self
            .store
            .find_references_by_name(symbol_name, limit.saturating_add(symbols.len()))?;

        if references.is_empty() {
            let usage_results =
                self.search_text(&Query::new(symbol_name), limit, &SearchOptions::default())?;
            let mut seen = std::collections::HashSet::new();
            for r in &results {
                seen.insert((r.file_path.clone(), r.start_line));
            }
            for mut result in usage_results {
                let key = (result.file_path.clone(), result.start_line);
                if seen.insert(key) {
                    result.kind = SearchResultKind::Reference;
                    result.metadata.match_type = Some("text".to_string());
                    results.push(result);
                }
            }
        } else {
            let mut file_lines: HashMap<String, Vec<String>> = HashMap::new();
            for reference in references {
                let covered = reference.kind == "definition"
                    && definition_spans.iter().any(|(path, start, end)| {
                        *path == reference.file_path && (*start..=*end).contains(&reference.line)
                    });
                if covered {
                    continue;
                }

                let line = reference.line as usize;
                let content = self.line_of(&mut file_lines, &reference.file_path, line);
                let kind = if reference.kind == "definition" {
                    SearchResultKind::Symbol
                } else {
                    SearchResultKind::Reference
                };
                results.push(
                    SearchResult::new(kind, reference.file_path, line, line, content, 1.0)
                        .with_metadata(SearchResultMetadata {
                            symbol_name: Some(symbol_name.to_string()),
                            match_type: Some(reference.kind),
                            ..Default::default()
                        }),
                );
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

    /// Text of one line (1-based), reading each file at most once per call.
    /// Unreadable files yield an empty snippet rather than failing the lookup.
    fn line_of(
        &self,
        cache: &mut HashMap<String, Vec<String>>,
        file_path: &str,
        line: usize,
    ) -> String {
        let lines = cache.entry(file_path.to_string()).or_insert_with(|| {
            self.read_file_lines(file_path, 1, usize::MAX)
                .map(|content| content.lines().map(str::to_string).collect())
                .unwrap_or_default()
        });
        lines
            .get(line.saturating_sub(1))
            .map(|l| l.trim().to_string())
            .unwrap_or_default()
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
    ///
    /// Uses `get_dependents_with_source_path` so the source file path is
    /// resolved as part of the same query (single JOIN), avoiding the
    /// previous N+1 lookup pattern.
    pub fn get_dependents(&self, file_path: &str) -> Result<Vec<DependencyInfo>> {
        let records = self.store.get_dependents_with_source_path(file_path)?;

        let deps = records
            .into_iter()
            .map(|(record, source_path)| DependencyInfo {
                target_path: source_path,
                import_name: record.import_name,
                kind: record.kind,
            })
            .collect();

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

        // Usages come from the AST `refs` table. Names it does not know
        // (data-file keys) fall back to counting text occurrences, minus the
        // known definitions; that count is capped and reads as "at least N".
        let usage_count = if self
            .store
            .find_references_by_name(symbol_name, 1)?
            .is_empty()
        {
            const USAGE_SEARCH_CAP: usize = 1000;
            let usage_results = self.search_text(
                &Query::new(symbol_name),
                USAGE_SEARCH_CAP,
                &SearchOptions::default(),
            )?;
            usage_results.len().saturating_sub(symbols.len())
        } else {
            self.store.count_usages(symbol_name)?
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
