//! Index a project directory, in two phases.
//!
//! Phase 1 (structure) parses every new or changed file and stores its
//! symbols, references, call edges, type relations, imports and chunks: every
//! command but `search` is usable once it is done. Phase 2 (embeddings) then
//! embeds the chunks still without a vector, and can be interrupted and
//! resumed (`--embeddings-only`).

use anyhow::{Result, bail};
use semantiq_embeddings::create_embedding_model;
use semantiq_index::embedder::DEFAULT_BATCH_SIZE;
use semantiq_index::{AutoIndexer, EmbeddingProgress, IndexStore, embed_pending};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use super::common::{resolve_db_path, resolve_project_root, warn_if_semantic_search_unavailable};

/// Which indexing phases to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phases {
    /// Structure, then embeddings.
    All,
    /// Structure only (`--no-embeddings`): chunks stay pending for phase 2.
    StructureOnly,
    /// Embed the pending chunks only (`--embeddings-only`).
    EmbeddingsOnly,
}

/// How often phase 2 logs its progress.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) async fn index(path: &Path, database: Option<PathBuf>, force: bool) -> Result<()> {
    index_with(path, database, force, Phases::All).await
}

pub(crate) async fn index_with(
    path: &Path,
    database: Option<PathBuf>,
    force: bool,
    phases: Phases,
) -> Result<()> {
    let project_root = resolve_project_root(path)?;
    let db_path = resolve_db_path(database, &project_root);

    info!("Indexing project: {:?}", project_root);
    info!("Database: {:?}", db_path);

    let start = Instant::now();
    let store = Arc::new(IndexStore::open(&db_path)?);

    // Check if parser version changed and prepare for full reindex if needed
    let rebuilt = store.check_and_prepare_for_reindex()?;

    if phases == Phases::EmbeddingsOnly {
        if store.get_stats()?.file_count == 0 {
            bail!(
                "The index at {} is empty{}: run `semantiq index` first.",
                db_path.display(),
                if rebuilt { " (its format changed)" } else { "" }
            );
        }
    } else {
        // `--force` rebuilds from scratch, embeddings included.
        if force && !rebuilt {
            store.clear_all_data()?;
        }
        index_structure(&store, &project_root)?;
    }

    if phases == Phases::StructureOnly {
        let pending = store.embedding_counts()?.pending();
        if pending > 0 {
            info!(
                "{} chunks without embeddings: `semantiq index --embeddings-only` (or `semantiq serve`) computes them",
                pending
            );
        }
    } else {
        warn_if_semantic_search_unavailable();
        index_embeddings(&store)?;
    }

    info!("  Time: {:.2}s", start.elapsed().as_secs_f64());
    Ok(())
}

/// Phase 1: parse new and changed files, drop vanished ones.
fn index_structure(store: &Arc<IndexStore>, project_root: &Path) -> Result<()> {
    let start = Instant::now();
    let indexer = AutoIndexer::without_watcher(Arc::clone(store), project_root.to_path_buf())?;
    let result = indexer.initial_index_with(&mut |progress| {
        // Progress update every 100 files
        if progress.indexed.is_multiple_of(100) {
            info!("Indexed {} files...", progress.indexed);
        }
    })?;
    let stats = store.get_stats()?;

    info!(
        "Structure ready in {:.2}s (refs, calls, impact, hierarchy, dead-code, deps, explain, map)",
        start.elapsed().as_secs_f64()
    );
    info!(
        "  Files: {} indexed, {} unchanged, {} removed",
        result.indexed, result.skipped, result.removed
    );
    info!("  Symbols: {}", stats.symbol_count);
    info!("  Chunks: {}", stats.chunk_count);
    info!("  Dependencies: {}", stats.dependency_count);
    info!("  Errors: {}", result.errors);
    Ok(())
}

/// Phase 2: embed every chunk that has no embedding yet. Each batch is
/// committed on its own, so an interrupted run resumes where it stopped.
fn index_embeddings(store: &IndexStore) -> Result<()> {
    let counts = store.embedding_counts()?;
    if counts.pending() == 0 {
        info!("Embeddings: all {} chunks already embedded", counts.total);
        return Ok(());
    }

    let model = match create_embedding_model(None) {
        Ok(model) => {
            info!("Embedding model loaded (dim={})", model.dimension());
            model
        }
        Err(e) => {
            warn!(
                "Could not load embedding model: {}. {} chunks left without embeddings.",
                e,
                counts.pending()
            );
            return Ok(());
        }
    };

    info!(
        "Embedding {} chunks (interrupting is safe: `semantiq index --embeddings-only` resumes)",
        counts.pending()
    );
    let start = Instant::now();
    let progress = EmbeddingProgress::new();
    let mut last_report = Instant::now();
    let result = embed_pending(
        store,
        model.as_ref(),
        DEFAULT_BATCH_SIZE,
        &progress,
        &|| false,
        &mut |progress| {
            if last_report.elapsed() >= PROGRESS_INTERVAL {
                let counts = progress.counts();
                info!(
                    "Embeddings: {}/{} chunks ({}%)",
                    counts.embedded,
                    counts.total,
                    counts.percent()
                );
                last_report = Instant::now();
            }
        },
    )?;

    let elapsed = start.elapsed().as_secs_f64();
    info!(
        "Embeddings ready in {:.2}s: {} chunks embedded ({:.1} chunks/s)",
        elapsed,
        result.embedded,
        result.embedded as f64 / elapsed.max(f64::EPSILON)
    );
    if result.failed > 0 {
        warn!(
            "{} chunks could not be embedded; `semantiq index --embeddings-only` retries them",
            result.failed
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_project(root: &Path) {
        fs::write(
            root.join("lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\npub fn twice(x: i32) -> i32 { add(x, x) }\n",
        )
        .unwrap();
        fs::write(root.join("main.rs"), "fn main() { let _ = twice(2); }\n").unwrap();
    }

    /// `--no-embeddings` stores the structure (refs, call edges) and leaves
    /// every chunk pending; `--embeddings-only` then embeds all of them.
    #[tokio::test]
    async fn test_structure_then_embeddings_only() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_project(root);
        let db = root.join("index.db");

        index_with(root, Some(db.clone()), false, Phases::StructureOnly)
            .await
            .unwrap();
        let store = IndexStore::open(&db).unwrap();
        assert_eq!(store.get_stats().unwrap().file_count, 2);
        assert!(!store.find_references_by_name("add", 10).unwrap().is_empty());
        let counts = store.embedding_counts().unwrap();
        assert!(counts.total > 0);
        assert_eq!(counts.embedded, 0, "phase 1 must not embed");

        index_with(root, Some(db.clone()), false, Phases::EmbeddingsOnly)
            .await
            .unwrap();
        let counts = store.embedding_counts().unwrap();
        assert_eq!(counts.pending(), 0);
        assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
    }

    /// Structural queries answer from phase 1 alone, and search answers while
    /// phase 2 is partway through.
    #[tokio::test]
    async fn test_queries_without_embeddings() {
        use semantiq_mcp::server::{
            CallsParams, FindRefsParams, ImpactParams, SearchParams, calls_output,
            find_refs_output, impact_output, search_output,
        };
        use semantiq_retrieval::RetrievalEngine;

        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write_project(&root);
        let db = root.join("index.db");
        index_with(&root, Some(db.clone()), false, Phases::StructureOnly)
            .await
            .unwrap();

        let store = Arc::new(IndexStore::open(&db).unwrap());
        assert_eq!(store.embedding_counts().unwrap().embedded, 0);
        let root_str = root.to_str().unwrap();
        let engine = RetrievalEngine::without_embeddings(Arc::clone(&store), root_str);

        let refs = find_refs_output(
            &engine,
            FindRefsParams {
                symbol: "add".to_string(),
                limit: Some(10),
            },
        )
        .unwrap();
        assert!(
            serde_json::to_string(&refs).unwrap().contains("main.rs")
                || serde_json::to_string(&refs).unwrap().contains("lib.rs")
        );

        let calls = calls_output(
            &engine,
            CallsParams {
                symbol: "add".to_string(),
                direction: Some("callers".to_string()),
                file_path: None,
                max_depth: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(
            serde_json::to_string(&calls).unwrap().contains("twice"),
            "{}",
            serde_json::to_string(&calls).unwrap()
        );

        let impact = impact_output(
            &engine,
            ImpactParams {
                symbol: "add".to_string(),
                file_path: None,
                max_depth: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(
            serde_json::to_string(&impact).unwrap().contains("twice"),
            "{}",
            serde_json::to_string(&impact).unwrap()
        );

        // Phase 2 interrupted after one chunk: search still answers.
        let model = semantiq_embeddings::StubEmbeddingModel::new();
        let batches = std::sync::atomic::AtomicUsize::new(0);
        embed_pending(
            &store,
            &model,
            1,
            &EmbeddingProgress::new(),
            &|| batches.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 1,
            &mut |_| {},
        )
        .unwrap();
        let counts = store.embedding_counts().unwrap();
        assert!(counts.embedded == 1 && counts.pending() > 0);
        let engine = RetrievalEngine::new(Arc::clone(&store), root_str);
        let search = search_output(
            &engine,
            SearchParams {
                query: "twice".to_string(),
                limit: Some(5),
                min_score: None,
                file_type: None,
                symbol_kind: None,
            },
        )
        .unwrap();
        assert!(
            serde_json::to_string(&search).unwrap().contains("twice"),
            "{}",
            serde_json::to_string(&search).unwrap()
        );
    }

    /// A full run embeds everything; a second run has nothing left to do.
    #[tokio::test]
    async fn test_full_index_embeds_all_chunks() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_project(root);
        let db = root.join("index.db");

        index(root, Some(db.clone()), false).await.unwrap();
        let store = IndexStore::open(&db).unwrap();
        let counts = store.embedding_counts().unwrap();
        assert!(counts.total > 0);
        assert_eq!(counts.pending(), 0);

        // `--force` rebuilds, embeddings included.
        index(root, Some(db.clone()), true).await.unwrap();
        assert_eq!(store.embedding_counts().unwrap().pending(), 0);
        assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
    }

    /// `--embeddings-only` on an empty index points at a full index run.
    #[tokio::test]
    async fn test_embeddings_only_on_empty_index_fails() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let db = root.join("index.db");
        let err = index_with(root, Some(db), false, Phases::EmbeddingsOnly)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("semantiq index"));
    }
}
