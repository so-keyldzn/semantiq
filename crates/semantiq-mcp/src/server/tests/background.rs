//! Background tasks started by `start_auto_indexer`: the embedding model's
//! availability (#52).

use semantiq_index::{BackgroundEmbedder, EmbedderConfig};
use std::sync::Arc;
use std::time::Duration;

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
