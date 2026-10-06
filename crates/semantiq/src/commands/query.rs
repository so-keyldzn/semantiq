//! Query commands (`search`, `refs`, `deps`, `explain`, `impact`, `map`,
//! `calls`, `hierarchy`, `dead-code`).
//!
//! Each command opens an existing index, refreshes files changed since the
//! last run (unless `--no-refresh`), then prints the same output as the
//! matching MCP tool: its markdown rendering by default, or its structured
//! output with `--json`.

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use ignore::WalkBuilder;
use semantiq_index::exclusions::is_file_too_large;
use semantiq_index::paths::to_relative_string;
use semantiq_index::{AutoIndexer, IndexStore, should_exclude_entry, should_exclude_path};
use semantiq_mcp::server::{
    CallsParams, DeadCodeParams, DepsParams, ExplainParams, FindRefsParams, HierarchyParams,
    ImpactParams, RepoMapParams, SearchParams, calls_output, dead_code_output, deps_output,
    explain_output, find_refs_output, hierarchy_output, impact_output, repo_map_output,
    search_output,
};
use semantiq_parser::Language;
use semantiq_retrieval::RetrievalEngine;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::common::DEFAULT_DB_NAME;

/// Index location and freshness options shared by the query commands.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct IndexArgs {
    /// Path to the database file (default: .semantiq.db in the current
    /// directory or the nearest parent that has one)
    #[arg(short, long)]
    pub database: Option<PathBuf>,

    /// Project root (default: the directory containing the database)
    #[arg(short, long)]
    pub project: Option<PathBuf>,

    /// Answer from the index as is, without reindexing changed files first
    #[arg(long)]
    pub no_refresh: bool,
}

/// What the command needs from the engine.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    /// Load the embedding model (semantic search).
    WithEmbeddings,
    /// Skip the model: only `search` uses it.
    WithoutEmbeddings,
}

pub(crate) struct SearchArgs {
    pub query: String,
    pub limit: usize,
    pub min_score: Option<f32>,
    pub file_type: Option<String>,
    pub symbol_kind: Option<String>,
    pub snippets: bool,
}

pub(crate) fn search(index: &IndexArgs, args: SearchArgs, json: bool) -> Result<()> {
    let (engine, _) = open_engine(index, Engine::WithEmbeddings)?;
    let output = search_output(
        &engine,
        SearchParams {
            query: args.query,
            limit: Some(args.limit),
            min_score: args.min_score,
            file_type: args.file_type,
            symbol_kind: args.symbol_kind,
            snippets: Some(args.snippets),
        },
    )
    .map_err(|e| anyhow!(e))?;

    // Persist sampled distance observations for `semantiq calibrate`.
    if let Err(e) = engine.flush_observations() {
        tracing::debug!("Failed to flush observations: {}", e);
    }

    print_output(&output, json, || output.render())
}

pub(crate) fn refs(index: &IndexArgs, symbol: String, limit: usize, json: bool) -> Result<()> {
    let (engine, _) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = find_refs_output(
        &engine,
        FindRefsParams {
            symbol,
            limit: Some(limit),
        },
    )
    .map_err(|e| anyhow!(e))?;
    print_output(&output, json, || output.render())
}

pub(crate) fn deps(index: &IndexArgs, file: &str, json: bool) -> Result<()> {
    let (engine, root) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = deps_output(
        &engine,
        DepsParams {
            file_path: normalize_file_arg(file, &root),
        },
    )
    .map_err(|e| anyhow!(e))?;
    print_output(&output, json, || output.render())
}

pub(crate) fn explain(index: &IndexArgs, symbol: String, json: bool) -> Result<()> {
    let (engine, _) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = explain_output(&engine, ExplainParams { symbol }).map_err(|e| anyhow!(e))?;
    print_output(&output, json, || output.render())
}

pub(crate) struct ImpactArgs {
    pub symbol: String,
    pub file: Option<String>,
    pub max_depth: Option<usize>,
    pub limit: Option<usize>,
}

pub(crate) fn impact(index: &IndexArgs, args: ImpactArgs, json: bool) -> Result<()> {
    let (engine, root) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = impact_output(
        &engine,
        ImpactParams {
            symbol: args.symbol,
            file_path: args.file.map(|f| normalize_file_arg(&f, &root)),
            max_depth: args.max_depth,
            limit: args.limit,
        },
    )
    .map_err(|e| anyhow!(e))?;
    print_output(&output, json, || output.render())
}

pub(crate) fn map(
    index: &IndexArgs,
    max_tokens: usize,
    focus: Vec<String>,
    path_prefix: Option<String>,
    json: bool,
) -> Result<()> {
    let (engine, _root) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = repo_map_output(
        &engine,
        RepoMapParams {
            max_tokens: Some(max_tokens),
            focus: Some(focus),
            path_prefix,
        },
    )
    .map_err(|e| anyhow!(e))?;
    if !json && !output.unmatched_focus.is_empty() {
        eprintln!(
            "No indexed file or symbol matches: {}",
            output.unmatched_focus.join(", ")
        );
    }
    print_output(&output, json, || output.render())
}

pub(crate) struct CallsArgs {
    pub symbol: String,
    pub direction: Option<String>,
    pub file: Option<String>,
    pub max_depth: Option<usize>,
    pub limit: Option<usize>,
}

pub(crate) fn calls(index: &IndexArgs, args: CallsArgs, json: bool) -> Result<()> {
    let (engine, root) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = calls_output(
        &engine,
        CallsParams {
            symbol: args.symbol,
            direction: args.direction,
            file_path: args.file.map(|f| normalize_file_arg(&f, &root)),
            max_depth: args.max_depth,
            limit: args.limit,
        },
    )
    .map_err(|e| anyhow!(e))?;
    print_output(&output, json, || output.render())
}

pub(crate) fn hierarchy(
    index: &IndexArgs,
    symbol: String,
    max_depth: Option<usize>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let (engine, _) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = hierarchy_output(
        &engine,
        HierarchyParams {
            symbol,
            max_depth,
            limit,
        },
    )
    .map_err(|e| anyhow!(e))?;
    print_output(&output, json, || output.render())
}

pub(crate) struct DeadCodeArgs {
    pub path_prefix: Option<String>,
    pub language: Option<String>,
    pub include_public: bool,
    pub limit: Option<usize>,
}

pub(crate) fn dead_code(index: &IndexArgs, args: DeadCodeArgs, json: bool) -> Result<()> {
    let (engine, _) = open_engine(index, Engine::WithoutEmbeddings)?;
    let output = dead_code_output(
        &engine,
        DeadCodeParams {
            path_prefix: args.path_prefix,
            language: args.language,
            include_public: Some(args.include_public),
            limit: args.limit,
        },
    )
    .map_err(|e| anyhow!(e))?;
    print_output(&output, json, || output.render())
}

fn print_output<T: Serialize>(
    output: &T,
    json: bool,
    render: impl FnOnce() -> String,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(output)?);
    } else {
        println!("{}", render().trim_end());
    }
    Ok(())
}

/// Turn a file argument into the project-relative path stored in the index:
/// accepts `./src/a.rs` and absolute paths inside the project.
fn normalize_file_arg(file: &str, root: &Path) -> String {
    let path = Path::new(file);
    let relative = if path.is_absolute() {
        path.canonicalize()
            .ok()
            .and_then(|abs| abs.strip_prefix(root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let relative = relative.to_string_lossy().replace('\\', "/");
    relative.trim_start_matches("./").to_string()
}

/// Locate and open the index, refresh it, and build the engine.
fn open_engine(index: &IndexArgs, mode: Engine) -> Result<(RetrievalEngine, PathBuf)> {
    let (db_path, root) = locate_index(index)?;
    let store = Arc::new(IndexStore::open(&db_path)?);

    if store.get_stats()?.file_count == 0 {
        bail!(
            "The Semantiq index at {} is empty. Run `semantiq index {}` first.",
            db_path.display(),
            root.display()
        );
    }

    if !index.no_refresh {
        refresh(&store, &root)?;
    }

    let root_str = root
        .to_str()
        .context("Project root path contains invalid UTF-8")?;
    let engine = match mode {
        Engine::WithEmbeddings => RetrievalEngine::new(store, root_str),
        Engine::WithoutEmbeddings => RetrievalEngine::without_embeddings(store, root_str),
    };
    Ok((engine, root))
}

/// Resolve `(database, project root)` from the flags, or by looking for
/// `.semantiq.db` in the current directory and its parents. Never creates a
/// database: a missing index is an error pointing at `semantiq index`.
fn locate_index(index: &IndexArgs) -> Result<(PathBuf, PathBuf)> {
    let cwd = std::env::current_dir()?;
    let (db_path, root) = match (&index.database, &index.project) {
        (Some(db), project) => {
            let db = cwd.join(db);
            let root = match project {
                Some(p) => cwd.join(p),
                None => db.parent().map(Path::to_path_buf).unwrap_or(cwd.clone()),
            };
            (db, root)
        }
        (None, Some(project)) => {
            let root = cwd.join(project);
            (root.join(DEFAULT_DB_NAME), root)
        }
        (None, None) => match cwd
            .ancestors()
            .find(|dir| dir.join(DEFAULT_DB_NAME).is_file())
        {
            Some(dir) => (dir.join(DEFAULT_DB_NAME), dir.to_path_buf()),
            None => bail!(
                "No Semantiq index found in {} or its parents. Run `semantiq index` \
                 (or `semantiq init`) at the project root first.",
                cwd.display()
            ),
        },
    };

    if !db_path.is_file() {
        bail!(
            "Semantiq index not found at {}. Run `semantiq index {}` first.",
            db_path.display(),
            root.display()
        );
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("Failed to resolve project root {}", root.display()))?;
    Ok((db_path, root))
}

/// Bring the index up to date before answering. The cheap check runs first so
/// an unchanged project never pays for the embedding model the indexer loads.
fn refresh(store: &Arc<IndexStore>, root: &Path) -> Result<()> {
    let rebuild = store.check_and_prepare_for_reindex()?;
    if rebuild {
        eprintln!("semantiq: index format changed, rebuilding the index (one time)...");
    } else if !index_is_stale(store, root)? {
        return Ok(());
    }

    let indexer = AutoIndexer::new(Arc::clone(store), root.to_path_buf())?;
    let result = indexer.initial_index()?;
    eprintln!(
        "semantiq: refreshed index ({} files reindexed, {} removed{})",
        result.indexed,
        result.removed,
        if result.errors > 0 {
            format!(", {} errors", result.errors)
        } else {
            String::new()
        }
    );
    Ok(())
}

/// Whether `AutoIndexer::initial_index` would change anything: a file it
/// indexes is new or modified, or an indexed file is gone. Mirrors its walk and
/// filters, and stops at the first difference.
fn index_is_stale(store: &IndexStore, root: &Path) -> Result<bool> {
    let mut vanished: HashSet<String> = store.list_file_paths()?.into_iter().collect();

    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                return !should_exclude_entry(&entry.file_name().to_string_lossy());
            }
            true
        })
        .build();

    for entry in walker.flatten() {
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        if Language::from_path(path).is_none() {
            continue;
        }
        let rel_path = to_relative_string(path, root);
        vanished.remove(&rel_path);

        // The indexer never stores these: they cannot make the index stale,
        // unless an older version stored one (it must then be removed).
        if should_exclude_path(Path::new(&rel_path)) || is_file_too_large(path) {
            if store.get_file_by_path(&rel_path)?.is_some() {
                tracing::debug!("Index stale: {} is no longer indexable", rel_path);
                return Ok(true);
            }
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        if store.needs_reindex(&rel_path, &content).unwrap_or(true) {
            tracing::debug!("Index stale: {} changed", rel_path);
            return Ok(true);
        }
    }

    if let Some(path) = vanished.iter().next() {
        tracing::debug!("Index stale: {} removed", path);
    }
    Ok(!vanished.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_file_arg() {
        let root = Path::new("/project");
        assert_eq!(normalize_file_arg("src/a.rs", root), "src/a.rs");
        assert_eq!(normalize_file_arg("./src/a.rs", root), "src/a.rs");
        // Absolute paths outside the project are left as is and match nothing.
        assert_eq!(
            normalize_file_arg("/elsewhere/a.rs", root),
            "/elsewhere/a.rs"
        );
    }
}
