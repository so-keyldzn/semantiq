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

/// Default number of `semantiq_search` results.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;
/// Default number of `semantiq_find_refs` references.
pub const DEFAULT_REFS_LIMIT: usize = 30;
const MAX_LIMIT: usize = 1000;

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
    let limit = params
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_LIMIT);
    let snippets = params.snippets.unwrap_or(false);

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

    // One extra result tells whether the list is truncated.
    let mut results = engine
        .search(&query, limit + 1, Some(options))
        .map_err(|e| {
            error!("Search failed: {}", e);
            "Search failed: an internal error occurred".to_string()
        })?
        .results;
    let truncated = results.len() > limit;
    results.truncate(limit);

    let terms = query_terms(&query);
    Ok(SearchOutput {
        results: results
            .into_iter()
            .map(|r| SearchHit {
                preview: preview_line(&r.content, r.metadata.symbol_name.as_deref(), &terms),
                file_path: r.file_path,
                start_line: r.start_line,
                end_line: r.end_line,
                score: (f64::from(r.score) * 100.0).round() / 100.0,
                symbol_name: r.metadata.symbol_name,
                symbol_kind: r.metadata.symbol_kind,
                content: snippets.then_some(r.content),
            })
            .collect(),
        query,
        truncated,
    })
}

/// Words too common in natural-language queries to locate a line.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "where", "what", "when", "which", "how", "are", "is", "does",
    "into", "from", "that", "this", "code", "file", "files",
];

/// Inflections dropped from query words so "debounced" finds "debounce".
const SUFFIXES: &[&str] = &["ing", "ed", "es", "er", "s"];

/// Lowercase query words worth looking for in a line, reduced to a stem.
fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOP_WORDS.contains(&w.as_str()))
        .map(|w| {
            SUFFIXES
                .iter()
                .find_map(|suffix| w.strip_suffix(suffix).filter(|stem| stem.len() >= 4))
                .map(str::to_string)
                .unwrap_or(w)
        })
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

fn is_comment_line(line: &str) -> bool {
    ["//", "/*", "*", "#", "--", "<!--"]
        .iter()
        .any(|marker| line.starts_with(marker))
}

/// The line of a hit that best shows why it matched: the line declaring the
/// symbol, else the line holding the most query terms (code before comments),
/// else the first line of code.
fn preview_line(content: &str, symbol: Option<&str>, terms: &[String]) -> String {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if let Some(symbol) = symbol
        && let Some(line) = lines
            .iter()
            .find(|l| !is_comment_line(l) && l.contains(symbol))
    {
        return clip_line(line);
    }

    let mut best: Option<(usize, &str)> = None;
    for line in &lines {
        let lower = line.to_lowercase();
        let hits = terms.iter().filter(|t| lower.contains(t.as_str())).count();
        if hits == 0 {
            continue;
        }
        // Two points per term, one more for code over comments.
        let score = hits * 2 + usize::from(!is_comment_line(line));
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, line));
        }
    }

    let line = best.map(|(_, l)| l).or_else(|| {
        lines
            .iter()
            .find(|l| !is_comment_line(l) && **l != "}")
            .or(lines.first())
            .copied()
    });
    line.map(clip_line).unwrap_or_default()
}

pub fn find_refs_output(
    engine: &RetrievalEngine,
    params: FindRefsParams,
) -> Result<FindRefsOutput, String> {
    debug!(symbol = %params.symbol, limit = ?params.limit, "semantiq_find_refs called");

    let symbol = validate_input(&params.symbol, "Symbol name")?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_REFS_LIMIT)
        .clamp(1, MAX_LIMIT);

    // One extra reference tells whether the list is truncated.
    let mut results = engine
        .find_references(&symbol, limit + 1)
        .map_err(|e| {
            error!("Find references failed: {}", e);
            "Find references failed: an internal error occurred".to_string()
        })?
        .results;
    let truncated = results.len() > limit;
    results.truncate(limit);

    let mut definitions = Vec::new();
    let mut usages = Vec::new();
    for r in results {
        let kind = r
            .metadata
            .match_type
            .unwrap_or_else(|| "reference".to_string());
        if kind == "definition" {
            // A definition's content is its whole source: keep the line
            // declaring the name.
            let content = preview_line(&r.content, Some(&symbol), &[]);
            definitions.push(Reference {
                file_path: r.file_path,
                line: r.start_line,
                kind: r.metadata.symbol_kind.unwrap_or(kind),
                content,
            });
        } else {
            usages.push(Reference {
                file_path: r.file_path,
                line: r.start_line,
                kind,
                content: clip_line(r.content.lines().next().unwrap_or("")),
            });
        }
    }

    Ok(FindRefsOutput {
        symbol,
        definitions,
        usages,
        truncated,
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
                    // The name is redundant when the path already ends with it
                    // (`crate::a::Name` imported as `Name`).
                    import_name: d
                        .import_name
                        .filter(|name| !d.target_path.ends_with(name.as_str())),
                    target_path: d.target_path,
                    kind: d.kind,
                    line: d.line,
                    end_line: d.end_line,
                })
                .collect()
        });
    let imported_by = engine
        .get_dependents(&file_path)
        .inspect_err(|e| error!("Could not analyze dependents: {}", e))
        .ok()
        .map(|deps| {
            // One row per importing statement: list each file once.
            let mut paths: Vec<String> = deps.into_iter().map(|d| d.target_path).collect();
            paths.sort();
            paths.dedup();
            paths
        });

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
            imported_in: Vec::new(),
            usage_count: 0,
            related_symbols: Vec::new(),
        });
    }

    // The engine collects related symbols in a HashSet; sort so the
    // truncated list is stable across calls.
    let mut related_symbols = explanation.related_symbols;
    related_symbols.sort();
    related_symbols.truncate(10);

    // Import statements are listed apart, without the module docs the index
    // attaches to them.
    let (imports, definitions): (Vec<_>, Vec<_>) = explanation
        .definitions
        .into_iter()
        .partition(|d| d.kind == "import");

    Ok(ExplainOutput {
        symbol: explanation.name,
        found: true,
        definitions: definitions
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
        imported_in: imports
            .into_iter()
            .map(|d| format!("{}:{}", d.file_path, d.start_line))
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
                rank: round_rank(f.rank),
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
                        rank: round_rank(s.rank),
                    })
                    .collect(),
            })
            .collect(),
        text: map.text,
    })
}

/// Ranks are only compared with each other: 4 significant digits are enough.
fn round_rank(rank: f64) -> f64 {
    if rank == 0.0 || !rank.is_finite() {
        return rank;
    }
    let scale = 10f64.powi(3 - rank.abs().log10().floor() as i32);
    (rank * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preview_prefers_the_symbol_declaration() {
        let content = "/// Escape a query for escape_fts5_query users.\n#[inline]\npub fn escape_fts5_query(q: &str) -> String {\n    q.replace('\"', \"\")\n}";
        assert_eq!(
            preview_line(content, Some("escape_fts5_query"), &[]),
            "pub fn escape_fts5_query(q: &str) -> String {"
        );
    }

    #[test]
    fn test_preview_picks_the_line_with_most_query_terms() {
        let content = "fn process(&self) {\n    // events are debounced here\n    let batch = self.debounce(events, DEBOUNCE_MS);\n}";
        let terms = query_terms("where are file watcher events debounced?");
        assert_eq!(terms, vec!["debounc", "event", "watch"]);
        assert_eq!(
            preview_line(content, None, &terms),
            "let batch = self.debounce(events, DEBOUNCE_MS);"
        );
    }

    #[test]
    fn test_preview_falls_back_to_first_code_line_and_clips() {
        let long = format!("let x = \"{}\";", "a".repeat(200));
        let content = format!("\n// header\n{}\n}}", long);
        let preview = preview_line(&content, None, &query_terms("nothing matches"));
        assert_eq!(preview.chars().count(), MAX_LINE_CHARS);
        assert!(preview.starts_with("let x = \"aaa") && preview.ends_with('…'));
        assert_eq!(preview_line("", None, &[]), "");
    }

    #[test]
    fn test_round_rank_keeps_four_significant_digits() {
        assert_eq!(round_rank(0.123456), 0.1235);
        assert_eq!(round_rank(0.000123456), 0.0001235);
        assert_eq!(round_rank(0.0), 0.0);
    }

    #[test]
    fn test_render_helpers() {
        let paths: Vec<String> = ["a::X", "b::Y", "a::Z", "./local.js", "std::io"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(join_paths(&paths), "a::{X, Z}, b::Y, ./local.js, std::io");

        let merged = merge_lines([
            (3, "call".to_string()),
            (5, "type".to_string()),
            (9, "call".to_string()),
        ]);
        assert_eq!(
            merged,
            vec![
                ("3,9".to_string(), "call".to_string()),
                ("5".to_string(), "type".to_string())
            ]
        );
        assert_eq!(location("a.rs", 4, 4), "a.rs:4");
        assert_eq!(location("a.rs", 4, 9), "a.rs:4-9");
    }
}
