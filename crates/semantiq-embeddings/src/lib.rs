pub mod model;

pub use model::{
    EmbeddingConfig, EmbeddingModel, Pooling, QUERY_PREFIX, StubEmbeddingModel,
    create_embedding_model, pool_and_normalize,
};

#[cfg(feature = "onnx")]
pub use model::ensure_models_downloaded;

/// Dimension of the embedding vectors (CodeRankEmbed produces 768-dim vectors).
/// Single source of truth: `semantiq-index` sizes `chunks_vec` from it.
pub const EMBEDDING_DIMENSION: usize = 768;

/// Identifier of the real ONNX model (CodeRankEmbed, pinned revisions).
pub const CODERANKEMBED_MODEL_ID: &str = "nomic-ai/CodeRankEmbed@3c4b608+onnx-int8@e74f446";

/// Identifier recorded by builds without the `onnx` feature, whose
/// `StubEmbeddingModel` stores zero vectors.
pub const STUB_EMBEDDING_MODEL_ID: &str = "stub";

/// Identifier of the embedding model this build writes into the index.
/// Persisted in the index metadata; any change (including stub -> ONNX)
/// forces `chunks_vec` to be rebuilt and the project to be fully re-indexed,
/// so zero vectors written by a stub build never survive a switch to ONNX.
#[cfg(feature = "onnx")]
pub const EMBEDDING_MODEL_ID: &str = CODERANKEMBED_MODEL_ID;
#[cfg(not(feature = "onnx"))]
pub const EMBEDDING_MODEL_ID: &str = STUB_EMBEDDING_MODEL_ID;
