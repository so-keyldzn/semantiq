pub mod model;

pub use model::{
    EMBEDDINGS_ENV_VAR, EmbeddingBackend, EmbeddingConfig, EmbeddingModel, Pooling, QUERY_PREFIX,
    StubEmbeddingModel, StubReason, create_embedding_model, pool_and_normalize, selected_backend,
};

#[cfg(feature = "onnx")]
pub use model::ensure_models_downloaded;

/// Dimension of the embedding vectors (CodeRankEmbed produces 768-dim vectors).
/// Single source of truth: `semantiq-index` sizes `chunks_vec` from it.
pub const EMBEDDING_DIMENSION: usize = 768;

/// Identifier of the real ONNX model (CodeRankEmbed, pinned revisions).
pub const CODERANKEMBED_MODEL_ID: &str = "nomic-ai/CodeRankEmbed@3c4b608+onnx-int8@e74f446";

/// Identifier recorded when the `StubEmbeddingModel` is selected (build
/// without `onnx`, or `SEMANTIQ_EMBEDDINGS=stub`): its vectors are all zero.
pub const STUB_EMBEDDING_MODEL_ID: &str = "stub";

/// Identifier of the embedding model this process writes into the index,
/// resolved at runtime from the build features and `SEMANTIQ_EMBEDDINGS`.
/// Persisted in the index metadata; any change (including stub <-> ONNX)
/// forces `chunks_vec` to be rebuilt and the project to be fully re-indexed,
/// so zero vectors written by the stub never survive a switch to ONNX.
pub fn embedding_model_id() -> &'static str {
    match selected_backend() {
        EmbeddingBackend::Onnx => CODERANKEMBED_MODEL_ID,
        EmbeddingBackend::Stub(_) => STUB_EMBEDDING_MODEL_ID,
    }
}

/// Why semantic search is unavailable in this process, or `None` when the
/// real model is selected.
pub fn semantic_search_unavailable_reason() -> Option<&'static str> {
    match selected_backend() {
        EmbeddingBackend::Onnx => None,
        EmbeddingBackend::Stub(reason) => Some(reason.describe()),
    }
}
