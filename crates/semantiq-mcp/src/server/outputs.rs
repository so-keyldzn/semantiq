//! Tool logic shared by the MCP server and the `semantiq` CLI.
//!
//! Each function validates its parameters, queries the engine and builds the
//! structured output, so both front-ends return exactly the same data. They are
//! synchronous (SQLite, ONNX, filesystem); the server runs them on the blocking
//! pool. `Err` strings are safe to show to the user: internal errors are logged
//! and replaced by an opaque message.

use semantiq_retrieval::{
    DEFAULT_IMPACT_DEPTH, DEFAULT_IMPACT_SITES, DEFAULT_REPO_MAP_TOKENS, RepoMapOptions,
    RetrievalEngine, SearchOptions,
};
use tracing::{debug, error};

use super::types::*;

const MAX_INPUT_LEN: usize = 500;
/// Maximum number of `focus` entries accepted by `semantiq_repo_map`.
const MAX_FOCUS_ENTRIES: usize = 50;

/// Trim and bound a required string argument.
pub(super) fn validate_input(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{} cannot be empty", label));
    }
    if value.len() > MAX_INPUT_LEN {
        return Err(format!(
            "{} exceeds maximum length of {} characters",
            label, MAX_INPUT_LEN
        ));
    }
    Ok(value.to_string())
}

/// Validate a project-relative file path, rejecting traversal attempts.
fn validate_file_path(value: &str) -> Result<String, String> {
    let path = validate_input(value, "File path")?;
    if path.contains("..") {
        return Err("File path must not contain '..'".to_string());
    }
    Ok(path)
}

pub fn search_output(
    engine: &RetrievalEngine,
    params: SearchParams,
) -> Result<SearchOutput, String> {
    debug!(
        query = %params.query,
        limit = ?params.limit,
        file_type = ?params.file_type,
        symbol_kind = ?params.symbol_kind,
        "semantiq_search called"
    );

    let query = validate_input(&params.query, "Query")?;
    let limit = params.limit.unwrap_or(20).min(1000);

    let mut options = SearchOptions::new();

    if let Some(score) = params.min_score {
        options = options.with_min_score(score);
    }

    if let Some(ref ft) = params.file_type {
        let types = SearchOptions::parse_csv(ft);
        if !types.is_empty() {
            options = options.with_file_types(types);
        }
    }

    if let Some(ref sk) = params.symbol_kind {
        let kinds = SearchOptions::parse_csv(sk);
        if !kinds.is_empty() {
            options = options.with_symbol_kinds(kinds);
        }
    }

    let results = engine.search(&query, limit, Some(options)).map_err(|e| {
        error!("Search failed: {}", e);
        "Search failed: an internal error occurred".to_string()
    })?;

    Ok(SearchOutput {
        query,
        total_count: results.total_count,
        search_time_ms: results.search_time_ms,
        results: results
            .results
            .into_iter()
            .map(|r| SearchHit {
                file_path: r.file_path,
                start_line: r.start_line,
                end_line: r.end_line,
                score: r.score,
                symbol_name: r.metadata.symbol_name,
                symbol_kind: r.metadata.symbol_kind,
                content: r.content,
            })
            .collect(),
    })
}

pub fn find_refs_output(
    engine: &RetrievalEngine,
    params: FindRefsParams,
) -> Result<FindRefsOutput, String> {
    debug!(symbol = %params.symbol, limit = ?params.limit, "semantiq_find_refs called");

    let symbol = validate_input(&params.symbol, "Symbol name")?;
    let limit = params.limit.unwrap_or(50).min(1000);

    let results = engine.find_references(&symbol, limit).map_err(|e| {
        error!("Find references failed: {}", e);
        "Find references failed: an internal error occurred".to_string()
    })?;

    let mut definitions = Vec::new();
    let mut usages = Vec::new();
    for r in results.results {
        let kind = r
            .metadata
            .match_type
            .unwrap_or_else(|| "reference".to_string());
        let is_definition = kind == "definition";
        let reference = Reference {
            file_path: r.file_path,
            line: r.start_line,
            kind,
            content: r.content,
        };
        if is_definition {
            definitions.push(reference);
        } else {
            usages.push(reference);
        }
    }

    Ok(FindRefsOutput {
        symbol,
        total_count: results.total_count,
        search_time_ms: results.search_time_ms,
        definitions,
        usages,
    })
}

pub fn deps_output(engine: &RetrievalEngine, params: DepsParams) -> Result<DepsOutput, String> {
    debug!(file = %params.file_path, "semantiq_deps called");

    let file_path = validate_file_path(&params.file_path)?;

    let imports = engine
        .get_dependencies(&file_path)
        .inspect_err(|e| error!("Could not analyze imports: {}", e))
        .ok()
        .map(|deps| {
            deps.into_iter()
                .map(|d| Import {
                    target_path: d.target_path,
                    import_name: d.import_name,
                    kind: d.kind,
                })
                .collect()
        });
    let imported_by = engine
        .get_dependents(&file_path)
        .inspect_err(|e| error!("Could not analyze dependents: {}", e))
        .ok()
        .map(|deps| deps.into_iter().map(|d| d.target_path).collect());

    Ok(DepsOutput {
        file_path,
        imports,
        imported_by,
    })
}

pub fn impact_output(
    engine: &RetrievalEngine,
    params: ImpactParams,
) -> Result<ImpactOutput, String> {
    debug!(symbol = %params.symbol, file = ?params.file_path, "semantiq_impact called");

    let symbol = validate_input(&params.symbol, "Symbol name")?;
    let file_path = match params.file_path {
        Some(ref path) => Some(validate_file_path(path)?),
        None => None,
    };
    let max_depth = params.max_depth.unwrap_or(DEFAULT_IMPACT_DEPTH);
    let limit = params.limit.unwrap_or(DEFAULT_IMPACT_SITES);

    let analysis = engine
        .analyze_impact(&symbol, file_path.as_deref(), max_depth, limit)
        .map_err(|e| {
            error!("Impact analysis failed: {}", e);
            "Impact analysis failed: an internal error occurred".to_string()
        })?;

    // Group sites by file, closest impact first.
    let mut files: Vec<ImpactedFile> = Vec::new();
    for site in &analysis.sites {
        let out = ImpactSiteOut {
            line: site.line,
            depth: site.depth,
            target: site.target.clone(),
            kind: site.kind.clone(),
            enclosing: site.enclosing.as_ref().map(|e| e.name.clone()),
            confidence: site.confidence.as_str().to_string(),
        };
        match files.iter_mut().find(|f| f.file_path == site.file_path) {
            Some(file) => {
                file.depth = file.depth.min(site.depth);
                file.is_test |= site.is_test;
                file.sites.push(out);
            }
            None => files.push(ImpactedFile {
                file_path: site.file_path.clone(),
                is_test: site.is_test,
                depth: site.depth,
                sites: vec![out],
            }),
        }
    }
    files.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.file_path.cmp(&b.file_path))
    });
    for file in &mut files {
        file.sites
            .sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.depth.cmp(&b.depth)));
    }
    let test_files = files
        .iter()
        .filter(|f| f.is_test)
        .map(|f| f.file_path.clone())
        .collect();

    Ok(ImpactOutput {
        symbol: analysis.symbol,
        definitions: analysis
            .definitions
            .into_iter()
            .map(|d| ImpactDefinitionOut {
                file_path: d.file_path,
                line: d.line,
                kind: d.kind,
            })
            .collect(),
        site_count: analysis.sites.len(),
        files,
        test_files,
        truncated: analysis.truncated,
    })
}

pub fn explain_output(
    engine: &RetrievalEngine,
    params: ExplainParams,
) -> Result<ExplainOutput, String> {
    debug!(symbol = %params.symbol, "semantiq_explain called");

    let symbol = validate_input(&params.symbol, "Symbol name")?;

    let explanation = engine.explain_symbol(&symbol).map_err(|e| {
        error!("Explain failed: {}", e);
        "Explain failed: an internal error occurred".to_string()
    })?;

    if !explanation.found {
        return Ok(ExplainOutput {
            symbol,
            found: false,
            definitions: Vec::new(),
            usage_count: 0,
            related_symbols: Vec::new(),
        });
    }

    // The engine collects related symbols in a HashSet; sort so the
    // truncated list is stable across calls.
    let mut related_symbols = explanation.related_symbols;
    related_symbols.sort();
    related_symbols.truncate(10);

    Ok(ExplainOutput {
        symbol: explanation.name,
        found: true,
        definitions: explanation
            .definitions
            .into_iter()
            .map(|d| Definition {
                file_path: d.file_path,
                kind: d.kind,
                start_line: d.start_line,
                end_line: d.end_line,
                signature: d.signature,
                doc_comment: d.doc_comment,
            })
            .collect(),
        usage_count: explanation.usage_count,
        related_symbols,
    })
}

pub fn repo_map_output(
    engine: &RetrievalEngine,
    params: RepoMapParams,
) -> Result<RepoMapOutput, String> {
    debug!(
        max_tokens = ?params.max_tokens,
        focus = ?params.focus,
        path_prefix = ?params.path_prefix,
        "semantiq_repo_map called"
    );

    let focus = params.focus.unwrap_or_default();
    if focus.len() > MAX_FOCUS_ENTRIES {
        return Err(format!(
            "focus accepts at most {} entries",
            MAX_FOCUS_ENTRIES
        ));
    }
    let focus = focus
        .iter()
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| validate_input(entry, "Focus entry"))
        .collect::<Result<Vec<_>, _>>()?;
    let path_prefix = match params.path_prefix {
        Some(ref prefix) if !prefix.trim().is_empty() => {
            let prefix = validate_input(prefix, "Path prefix")?;
            if prefix.contains("..") {
                return Err("Path prefix must not contain '..'".to_string());
            }
            Some(prefix)
        }
        _ => None,
    };
    let options = RepoMapOptions {
        max_tokens: params.max_tokens.unwrap_or(DEFAULT_REPO_MAP_TOKENS),
        focus,
        path_prefix,
    };

    let map = engine.repo_map(&options).map_err(|e| {
        error!("Repo map failed: {}", e);
        "Repo map failed: an internal error occurred".to_string()
    })?;

    Ok(RepoMapOutput {
        max_tokens: map.max_tokens,
        estimated_tokens: map.estimated_tokens,
        total_files: map.total_files,
        total_symbols: map.total_symbols,
        shown_symbols: map.shown_symbols,
        focus_files: map.focus_files,
        focus_symbols: map.focus_symbols,
        unmatched_focus: map.unmatched_focus,
        files: map
            .files
            .into_iter()
            .map(|f| RepoMapFileOut {
                file_path: f.path,
                language: f.language,
                rank: f.rank,
                symbols: f
                    .symbols
                    .into_iter()
                    .map(|s| RepoMapSymbolOut {
                        name: s.name,
                        kind: s.kind,
                        line: s.line,
                        parent: s.parent,
                        signature: s.signature,
                        doc: s.doc,
                        rank: s.rank,
                    })
                    .collect(),
            })
            .collect(),
        text: map.text,
    })
}
