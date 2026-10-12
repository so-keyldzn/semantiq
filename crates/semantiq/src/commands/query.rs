//! Query commands (`search`, `refs`, `deps`, `explain`, `impact`, `map`,
//! `calls`, `hierarchy`, `dead-code`).
//!
//! Each command opens an existing index, refreshes files changed since the
//! last run (unless `--no-refresh`), then prints the same output as the
//! matching MCP tool: its markdown rendering by default, or its structured
//! output with `--json`.

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use semantiq_index::embedder::DEFAULT_BATCH_SIZE;
use semantiq_index::exclusions::is_file_too_large;
use semantiq_index::paths::to_relative_string;
use semantiq_index::{
    AutoIndexer, EmbeddingProgress, IndexStore, WalkFailures, embed_pending, project_walker,
    read_error_is_final, should_exclude_path,
};
use semantiq_mcp::server::{
    CallsParams, DeadCodeParams, DepsParams, ExplainParams, FindRefsParams, HierarchyParams,
    ImpactParams, RepoMapParams, SearchParams, calls_output, dead_code_output, deps_output,
    explain_output, find_refs_output, hierarchy_output, impact_output, repo_map_output,
    search_output,
};
use semantiq_parser::Language;
use semantiq_retrieval::{RetrievalEngine, normalize_input_path};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::common::{DEFAULT_DB_NAME, print_stdout};

/// Index location and freshness options shared by the query commands.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct IndexArgs {
    /// Path to the database file (default: .semantiq.db in the current
    /// directory or the nearest parent that has one)
    #[arg(short, long)]
    pub database: Option<PathBuf>,

    /// Project root (default: the root the index was built from, recorded in
    /// the database; the current directory for an index that has none)
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
    let (engine, _, store) = open_engine_with_store(index, Engine::WithEmbeddings)?;
    complete_embeddings(&engine, &store)?;
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
        print_stdout(&serde_json::to_string_pretty(output)?)
    } else {
        print_stdout(render().trim_end())
    }
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
    normalize_input_path(&relative.to_string_lossy())
}

/// Pending chunks `search` embeds itself before answering: about what a
/// refresh of a few edited files leaves, a second or two of inference.
const INLINE_EMBEDDING_LIMIT: usize = 64;

/// Before a search, embed the chunks the refresh (phase 1 only) left without
/// an embedding when there are few of them. Otherwise, e.g. after
/// `semantiq index --no-embeddings` or during a first `serve`, answer with the
/// embeddings available and say how much of the semantic index is ready.
fn complete_embeddings(engine: &RetrievalEngine, store: &IndexStore) -> Result<()> {
    let Some(model) = engine.embedding_model() else {
        return Ok(());
    };
    if model.is_stub() {
        // The MCP server says so in its instructions; the CLI has nowhere
        // else to say it (#106).
        let reason = semantiq_embeddings::semantic_search_unavailable_reason()
            .unwrap_or("stub embedding model");
        eprintln!(
            "semantiq: semantic search unavailable ({reason}): only symbol and text matches \
             are returned"
        );
        return Ok(());
    }
    let counts = store.embedding_counts()?;
    if counts.pending() == 0 {
        return Ok(());
    }
    if counts.pending() <= INLINE_EMBEDDING_LIMIT {
        let result = embed_pending(
            store,
            model,
            DEFAULT_BATCH_SIZE,
            &EmbeddingProgress::new(),
            &|| false,
            &mut |_| {},
        )?;
        tracing::debug!("Embedded {} pending chunks", result.embedded);
        return Ok(());
    }
    eprintln!(
        "semantiq: semantic index {}% ready ({}/{} chunks embedded), searching with what is \
         ready: run `semantiq index --embeddings-only` to complete it",
        counts.percent(),
        counts.embedded,
        counts.total
    );
    Ok(())
}

/// Locate and open the index, refresh it, and build the engine.
fn open_engine(index: &IndexArgs, mode: Engine) -> Result<(RetrievalEngine, PathBuf)> {
    let (engine, root, _) = open_engine_with_store(index, mode)?;
    Ok((engine, root))
}

/// `open_engine`, also returning the store.
fn open_engine_with_store(
    index: &IndexArgs,
    mode: Engine,
) -> Result<(RetrievalEngine, PathBuf, Arc<IndexStore>)> {
    let (db_path, root) = locate_index(index)?;
    let store = Arc::new(IndexStore::open(&db_path)?);
    let root = match root {
        Some(root) => root,
        None => recorded_project_root(&store, &db_path)?,
    };

    if store.get_stats()?.file_count == 0 {
        bail!(
            "The Semantiq index at {} is empty. Run `semantiq index {}` first.",
            db_path.display(),
            root.display()
        );
    }

    if !index.no_refresh {
        refresh(&store, &db_path, &root)?;
    }

    let root_str = root
        .to_str()
        .context("Project root path contains invalid UTF-8")?;
    let engine = match mode {
        Engine::WithEmbeddings => RetrievalEngine::new(Arc::clone(&store), root_str),
        Engine::WithoutEmbeddings => {
            RetrievalEngine::without_embeddings(Arc::clone(&store), root_str)
        }
    };
    Ok((engine, root, store))
}

/// Resolve `(database, project root)` from the flags, or by looking for
/// `.semantiq.db` in the current directory and its parents. Never creates a
/// database: a missing index is an error pointing at `semantiq index`.
///
/// With `--database` alone the root is `None`: it is read from the database
/// once opened ([`recorded_project_root`]). Defaulting it to a directory
/// (the database's parent, as before) made the refresh walk a directory
/// holding none of the indexed files and prune them all (#32).
pub(super) fn locate_index(index: &IndexArgs) -> Result<(PathBuf, Option<PathBuf>)> {
    let cwd = std::env::current_dir()?;
    let (db_path, root) = match (&index.database, &index.project) {
        (Some(db), project) => (cwd.join(db), project.as_ref().map(|p| cwd.join(p))),
        (None, Some(project)) => {
            let root = cwd.join(project);
            (root.join(DEFAULT_DB_NAME), Some(root))
        }
        (None, None) => match cwd
            .ancestors()
            .find(|dir| dir.join(DEFAULT_DB_NAME).is_file())
        {
            Some(dir) => (dir.join(DEFAULT_DB_NAME), Some(dir.to_path_buf())),
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
            root.as_deref().unwrap_or(&cwd).display()
        );
    }
    let root = root
        .map(|root| {
            root.canonicalize()
                .with_context(|| format!("Failed to resolve project root {}", root.display()))
        })
        .transpose()?;
    Ok((db_path, root))
}

/// The project root an index was built from (`index` and `serve` record it),
/// for a `--database` given without `--project`. An index built by an older
/// version has none: the current directory is assumed, as `index` and `serve`
/// do, and `AutoIndexer::initial_index` refuses to prune a populated index
/// from a directory holding none of its files.
fn recorded_project_root(store: &IndexStore, db_path: &Path) -> Result<PathBuf> {
    match store.get_project_root()? {
        Some(root) => root.canonicalize().with_context(|| {
            format!(
                "The index at {} was built from {}, which no longer exists: pass \
                 `--project <root>`",
                db_path.display(),
                root.display()
            )
        }),
        None => std::env::current_dir()?
            .canonicalize()
            .context("Failed to resolve the current directory"),
    }
}

/// Bring the index up to date before answering: phase 1 (structure) only, no
/// embedding model. The cheap check runs first so an unchanged project never
/// pays for the parsers either.
///
/// A query command never re-points an index: when the database records a
/// project root and `root` is another directory, the refresh is refused
/// (`index`/`serve` warn and re-point, which is what the user asks them for).
fn refresh(store: &Arc<IndexStore>, db_path: &Path, root: &Path) -> Result<()> {
    if let Some(recorded) = store.get_project_root()?
        && recorded != root
    {
        bail!(
            "The index at {} was built from {}, not from {}: pass `--project {}`, or \
             rebuild it from this directory with `semantiq index {} --database {}`",
            db_path.display(),
            recorded.display(),
            root.display(),
            recorded.display(),
            root.display(),
            db_path.display()
        );
    }
    let rebuild = store.check_and_prepare_for_reindex()?;
    if rebuild {
        eprintln!("semantiq: index format changed, rebuilding the index (one time)...");
    } else if !index_is_stale(store, root)? {
        return Ok(());
    }

    let indexer = AutoIndexer::without_watcher(Arc::clone(store), root.to_path_buf())?;
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
/// filters, and stops at the first difference. Like the indexer, a walk that
/// could not cover the whole tree does not count unseen files as gone.
fn index_is_stale(store: &IndexStore, root: &Path) -> Result<bool> {
    let mut vanished: HashSet<String> = store.list_file_paths()?.into_iter().collect();
    let mut failures = WalkFailures::default();

    for entry in project_walker(root).build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                failures.record(&e, root);
                tracing::debug!("Walk error while checking the index: {}", e);
                continue;
            }
        };
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
        // A file that vanished or is not UTF-8 any more loses its row in the
        // indexer's walk: an existing row makes the index stale. Any other
        // read error keeps the row there, so it does not here either.
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) if read_error_is_final(&e) => {
                if store.get_file_by_path(&rel_path)?.is_some() {
                    tracing::debug!("Index stale: {} is no longer readable", rel_path);
                    return Ok(true);
                }
                continue;
            }
            Err(_) => continue,
        };
        if store.needs_reindex(&rel_path, &content).unwrap_or(true) {
            tracing::debug!("Index stale: {} changed", rel_path);
            return Ok(true);
        }
    }

    // Files under a directory the walk could not list did not necessarily
    // vanish: the indexer keeps their rows (`initial_index`), so they do not
    // make the index stale either.
    vanished.retain(|path| !failures.covers(path));
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
