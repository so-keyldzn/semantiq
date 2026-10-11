//! Background tasks started by `start_auto_indexer`: the fallback without a
//! file watcher (#47) and the embedding model's availability (#52).

use super::SemantiqServer;
use rmcp::ServerHandler;
use semantiq_index::{BackgroundEmbedder, EmbedderConfig, IndexStore};
use semantiq_retrieval::RetrievalEngine;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

async fn wait_until(timeout: Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !cond() {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// The server built when the file watcher fails (inotify limit, ...).
fn server_without_watcher(temp: &TempDir) -> SemantiqServer {
    let db_path = temp.path().join(".semantiq.db");
    let store = Arc::new(IndexStore::open(&db_path).unwrap());
    let project_root = temp.path().to_path_buf();
    let engine = Arc::new(RetrievalEngine::new(
        Arc::clone(&store),
        project_root.to_str().unwrap(),
    ));
    let (auto_indexer, watching) = SemantiqServer::indexer_without_watcher(
        &store,
        project_root,
        &anyhow::anyhow!("No space left on device (os error 28)"),
    );
    assert!(
        auto_indexer.is_some(),
        "falls back to an indexer without watcher"
    );
    assert!(!watching);
    SemantiqServer {
        engine,
        store,
        auto_indexer,
        watching,
        initial_indexing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        embedder: Arc::new(std::sync::OnceLock::new()),
    }
}

/// #47: without a watcher, the initial index and the embedder still run.
#[tokio::test]
async fn initial_index_and_embedder_run_without_a_file_watcher() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn alpha() -> i32 { 1 }\n\npub fn beta() -> i32 { alpha() + 1 }\n",
    )
    .unwrap();
    let server = server_without_watcher(&temp);

    server.start_auto_indexer();

    let store = Arc::clone(server.store());
    assert!(
        wait_until(Duration::from_secs(30), || {
            !server.is_initial_indexing()
                && store.get_stats().map(|s| s.file_count).unwrap_or(0) > 0
        })
        .await,
        "initial index ran"
    );
    assert!(server.embedder.get().is_some(), "embedder started");
    // The embedder is woken after phase 1 and fills the backlog (stub model).
    assert!(
        wait_until(Duration::from_secs(30), || {
            store
                .embedding_counts()
                .map(|c| c.pending() == 0)
                .unwrap_or(false)
        })
        .await,
        "pending chunks embedded"
    );
    assert!(!server.is_watching());
}

/// #47: the instructions say that edits are not picked up live.
#[test]
fn instructions_say_when_live_watching_is_off() {
    let temp = TempDir::new().unwrap();
    let server = server_without_watcher(&temp);
    let instructions = server.get_info().instructions.unwrap();
    assert!(
        instructions.contains("live file watching is off"),
        "{instructions}"
    );

    // No indexer at all (watcher and fallback both failed): the note says
    // auto-indexing is off, not merely live watching.
    let (server, _temp) = super::create_test_server();
    assert!(server.auto_indexer.is_none());
    let instructions = server.get_info().instructions.unwrap();
    assert!(
        instructions.contains("auto-indexing is off"),
        "{instructions}"
    );
    assert!(!instructions.contains("live file watching is off"));
}

/// #52: a model that fails to load is reported as the reason semantic search
/// is unavailable (search notice, `GET /stats`), and retried later.
#[tokio::test]
async fn failed_model_load_is_reported_as_unavailable_reason() {
    let (server, _temp) = super::create_test_server();
    assert!(server.embedding_model_error().is_none());

    let embedder = BackgroundEmbedder::spawn_with(
        Arc::clone(server.store()),
        Box::new(|| anyhow::bail!("sha256 mismatch for coderankembed-int8.onnx")),
        EmbedderConfig {
            poll_interval: Duration::from_secs(60),
            initial_retry_delay: Duration::from_secs(60),
            max_retry_delay: Duration::from_secs(60),
        },
    )
    .unwrap();
    embedder.notify();
    assert!(embedder.wait_idle(Duration::from_secs(10)));
    server.embedder.set(embedder).ok().unwrap();

    let error = server.embedding_model_error().unwrap();
    assert!(error.contains("sha256 mismatch"), "{error}");
    assert!(server.embedding_in_progress().is_none());

    let reason = server.semantic_search_unavailable_reason().unwrap();
    if semantiq_embeddings::semantic_search_unavailable_reason().is_some() {
        // Stub build (tests): the backend reason comes first.
        assert_eq!(
            reason,
            semantiq_embeddings::semantic_search_unavailable_reason().unwrap()
        );
    } else {
        assert!(reason.contains("sha256 mismatch"), "{reason}");
        assert!(reason.contains("retried"), "{reason}");
    }
    assert!(crate::server::model_error_reason(error).contains("retried in the background"));
}
